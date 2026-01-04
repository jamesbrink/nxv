// k6 load test script for nxv API server
//
// Usage:
//   # Start the server first
//   cargo run -- serve
//
//   # Run load test (default: 10 VUs for 30s)
//   k6 run scripts/load-test.js
//
//   # Custom VUs and duration
//   k6 run --vus 50 --duration 60s scripts/load-test.js
//
//   # With custom base URL
//   k6 run -e BASE_URL=http://localhost:3000 scripts/load-test.js
//
//   # Run specific scenario (stress, spike, soak)
//   k6 run -e SCENARIO=stress scripts/load-test.js
//   k6 run -e SCENARIO=spike scripts/load-test.js
//   k6 run -e SCENARIO=soak scripts/load-test.js
//
//   # Bypass CDN cache (adds unique query params to each request)
//   k6 run -e SCENARIO=stress -e BASE_URL=https://nxv.urandom.io -e NO_CACHE=1 scripts/load-test.js
//
//   # AGGRESSIVE scenarios to reproduce issue #14 (spawn_blocking exhaustion)
//   # aggressive: 200 VUs instantly, hammers version endpoints, NO sleep
//   k6 run -e SCENARIO=aggressive -e BASE_URL=https://nxv.urandom.io -e NO_CACHE=1 scripts/load-test.js
//
//   # hammer: 500 VUs, even more extreme
//   k6 run -e SCENARIO=hammer -e BASE_URL=https://nxv.urandom.io -e NO_CACHE=1 scripts/load-test.js

import http from 'k6/http';
import { check, sleep, group } from 'k6';
import { Rate, Trend, Counter } from 'k6/metrics';

// Custom metrics
const errorRate = new Rate('errors');
const searchLatency = new Trend('search_latency', true);
const packageLatency = new Trend('package_latency', true);
const historyLatency = new Trend('history_latency', true);
const statsLatency = new Trend('stats_latency', true);
const healthLatency = new Trend('health_latency', true);
const requestCount = new Counter('total_requests');

// Configuration
const BASE_URL = __ENV.BASE_URL || 'http://127.0.0.1:8080';
const API_BASE = `${BASE_URL}/api/v1`;
const NO_CACHE = __ENV.NO_CACHE === '1' || __ENV.NO_CACHE === 'true';

// Counter for unique cache-busting values
let requestCounter = 0;

// Add cache-busting query parameter to bypass CDN cache
function cacheBust(url) {
  if (!NO_CACHE) return url;
  const separator = url.includes('?') ? '&' : '?';
  // Use combination of timestamp, VU id, and counter for uniqueness
  const bust = `_cb=${Date.now()}-${__VU}-${requestCounter++}`;
  return `${url}${separator}${bust}`;
}

// Scenario definitions
const scenarios = {
  // Default: constant load
  constant: {
    executor: 'constant-vus',
    vus: 10,
    duration: '30s',
  },
  // Stress test: ramp up to find limits
  stress: {
    executor: 'ramping-vus',
    startVUs: 1,
    stages: [
      { duration: '30s', target: 10 },   // Ramp up to 10 users
      { duration: '1m', target: 50 },    // Ramp up to 50 users
      { duration: '30s', target: 100 },  // Ramp up to 100 users
      { duration: '1m', target: 100 },   // Stay at 100 users
      { duration: '30s', target: 0 },    // Ramp down
    ],
  },
  // Spike test: sudden traffic burst
  spike: {
    executor: 'ramping-vus',
    startVUs: 1,
    stages: [
      { duration: '10s', target: 5 },    // Normal load
      { duration: '5s', target: 100 },   // Spike!
      { duration: '30s', target: 100 },  // Hold spike
      { duration: '10s', target: 5 },    // Back to normal
      { duration: '10s', target: 0 },    // Ramp down
    ],
  },
  // Soak test: sustained load over time
  soak: {
    executor: 'constant-vus',
    vus: 20,
    duration: '5m',
  },
  // AGGRESSIVE: Designed to reproduce issue #14 (spawn_blocking exhaustion)
  // Instantly spawns 200 VUs hammering the exact endpoints that caused the production hang
  aggressive: {
    executor: 'constant-vus',
    vus: 200,
    duration: '2m',
    exec: 'aggressiveTest',
  },
  // HAMMER: Even more extreme - 500 VUs, no mercy
  hammer: {
    executor: 'ramping-vus',
    startVUs: 50,
    stages: [
      { duration: '10s', target: 200 },  // Quick ramp to 200
      { duration: '10s', target: 500 },  // Ramp to 500
      { duration: '1m', target: 500 },   // Hold at 500
      { duration: '10s', target: 0 },    // Ramp down
    ],
    exec: 'aggressiveTest',
  },
};

// Select scenario based on environment variable (default: constant)
const selectedScenario = __ENV.SCENARIO || 'constant';
if (!scenarios[selectedScenario]) {
  throw new Error(`Unknown scenario: ${selectedScenario}. Valid: constant, stress, spike, soak`);
}

export const options = {
  scenarios: {
    [selectedScenario]: scenarios[selectedScenario],
  },
  // Thresholds for pass/fail
  thresholds: {
    http_req_duration: ['p(95)<500', 'p(99)<1000'],  // 95th < 500ms, 99th < 1s
    http_req_failed: ['rate<0.01'],                  // Error rate < 1%
    errors: ['rate<0.05'],                           // Custom error rate < 5%
    search_latency: ['p(95)<400'],                   // Search 95th < 400ms
    health_latency: ['p(99)<100'],                   // Health check 99th < 100ms
  },
};

// Common search queries to test (mix of common and edge cases)
const SEARCH_QUERIES = [
  'python',
  'nodejs',
  'rust',
  'go',
  'vim',
  'neovim',
  'firefox',
  'chromium',
  'git',
  'gcc',
  'llvm',
  'docker',
  'kubernetes',
  'nginx',
  'postgresql',
  'redis',
  'hello',
  'coreutils',
  'bash',
  'zsh',
  // Edge cases
  'nonexistent-package-12345',  // Should return empty
  'a',                           // Very short query
  'python3',                     // With number
];

// Package attribute paths for detailed lookups
const PACKAGE_ATTRS = [
  'python311',
  'python312',
  'nodejs_20',
  'rustc',
  'go',
  'vim',
  'neovim',
  'firefox',
  'git',
  'hello',
];

// Helper to pick random item from array
function randomChoice(arr) {
  return arr[Math.floor(Math.random() * arr.length)];
}

// Helper to make request and track metrics
function apiRequest(name, url, metricTrend) {
  const start = Date.now();
  const finalUrl = cacheBust(url);
  const headers = { 'Accept': 'application/json' };

  // Add cache-control headers when bypassing cache
  if (NO_CACHE) {
    headers['Cache-Control'] = 'no-cache, no-store, must-revalidate';
    headers['Pragma'] = 'no-cache';
  }

  const res = http.get(finalUrl, {
    headers: headers,
    tags: { name: name },
  });
  const duration = Date.now() - start;

  requestCount.add(1);
  if (metricTrend) {
    metricTrend.add(duration);
  }

  const success = check(res, {
    'status is 200 or 404': (r) => r.status === 200 || r.status === 404,
    'response is JSON': (r) => {
      const ct = r.headers['Content-Type'] || '';
      return ct.includes('application/json');
    },
  });

  errorRate.add(!success);
  return res;
}

// Rustc versions that caused the original hang (from issue #14)
// The actual burst was 60+ requests for versions 1.21.0 through 1.89.0 in ~200ms
const RUSTC_VERSIONS = [
  '1.21.0', '1.22.0', '1.23.0', '1.24.0', '1.25.0', '1.26.0', '1.27.0', '1.28.0',
  '1.29.0', '1.30.0', '1.31.0', '1.32.0', '1.33.0', '1.34.0', '1.34.2', '1.35.0',
  '1.36.0', '1.37.0', '1.38.0', '1.39.0', '1.40.0', '1.41.0', '1.42.0', '1.43.0',
  '1.44.0', '1.45.0', '1.46.0', '1.47.0', '1.48.0', '1.49.0', '1.50.0', '1.51.0',
  '1.52.0', '1.53.0', '1.54.0', '1.55.0', '1.56.0', '1.57.0', '1.58.0', '1.59.0',
  '1.60.0', '1.61.0', '1.62.0', '1.63.0', '1.64.0', '1.65.0', '1.66.0', '1.67.0',
  '1.68.0', '1.69.0', '1.70.0', '1.71.0', '1.72.0', '1.73.0', '1.74.0', '1.75.0',
  '1.76.0', '1.77.0', '1.78.0', '1.79.0', '1.80.0', '1.81.0', '1.82.0', '1.83.0',
  '1.84.0', '1.84.1', '1.85.0', '1.86.0', '1.87.0', '1.88.0', '1.89.0',
];

// More packages to hammer
const AGGRESSIVE_PACKAGES = [
  'rustc', 'python311', 'python312', 'nodejs_20', 'nodejs_22',
  'go', 'gcc', 'llvm', 'firefox', 'chromium',
];

const AGGRESSIVE_VERSIONS = [
  '1.0', '2.0', '3.0', '3.11', '3.12', '20', '22', '1.21', '1.22',
  '14', '15', '16', '17', '18', '100', '110', '120',
];

// AGGRESSIVE test function - designed to reproduce issue #14
// Hammers version endpoints with NO sleep between requests
export function aggressiveTest() {
  const rand = Math.random();

  if (rand < 0.5) {
    // 50% - Hit the exact endpoints that caused the production hang
    const version = randomChoice(RUSTC_VERSIONS);
    const url = `${API_BASE}/packages/rustc/versions/${version}`;
    apiRequest('version_rustc', url, packageLatency);
  } else if (rand < 0.8) {
    // 30% - Hit version endpoints for various packages
    const pkg = randomChoice(AGGRESSIVE_PACKAGES);
    const version = randomChoice(AGGRESSIVE_VERSIONS);
    const url = `${API_BASE}/packages/${pkg}/versions/${version}`;
    apiRequest('version_pkg', url, packageLatency);
  } else if (rand < 0.9) {
    // 10% - History endpoints (also DB-heavy)
    const pkg = randomChoice(AGGRESSIVE_PACKAGES);
    const url = `${API_BASE}/packages/${pkg}/history`;
    apiRequest('history', url, historyLatency);
  } else {
    // 10% - Search (to mix in some different queries)
    const query = randomChoice(SEARCH_QUERIES);
    const url = `${API_BASE}/search?q=${encodeURIComponent(query)}&limit=50`;
    apiRequest('search', url, searchLatency);
  }

  // NO SLEEP - maximum aggression!
}

// Main test function
export default function() {
  // Randomly choose which endpoint to test (weighted)
  const rand = Math.random();

  if (rand < 0.4) {
    // 40% - Search requests (most common operation)
    group('search', () => {
      const query = randomChoice(SEARCH_QUERIES);
      const url = `${API_BASE}/search?q=${encodeURIComponent(query)}&limit=20`;
      const res = apiRequest('search', url, searchLatency);

      if (res.status === 200) {
        check(res, {
          'search has data array': (r) => {
            const body = JSON.parse(r.body);
            return Array.isArray(body.data);
          },
          'search has meta': (r) => {
            const body = JSON.parse(r.body);
            return body.meta !== undefined;
          },
        });
      }
    });
  } else if (rand < 0.55) {
    // 15% - Search with version filter
    group('search_with_version', () => {
      const queries = [
        { q: 'python', version: '3.11' },
        { q: 'nodejs', version: '20' },
        { q: 'rust', version: '1.7' },
        { q: 'go', version: '1.21' },
      ];
      const query = randomChoice(queries);
      const url = `${API_BASE}/search?q=${query.q}&version=${query.version}&limit=10`;
      apiRequest('search_version', url, searchLatency);
    });
  } else if (rand < 0.70) {
    // 15% - Package lookup
    group('package', () => {
      const attr = randomChoice(PACKAGE_ATTRS);
      const url = `${API_BASE}/packages/${encodeURIComponent(attr)}`;
      apiRequest('package', url, packageLatency);
    });
  } else if (rand < 0.80) {
    // 10% - Version history
    group('history', () => {
      const attr = randomChoice(PACKAGE_ATTRS);
      const url = `${API_BASE}/packages/${encodeURIComponent(attr)}/history`;
      apiRequest('history', url, historyLatency);
    });
  } else if (rand < 0.90) {
    // 10% - Stats
    group('stats', () => {
      apiRequest('stats', `${API_BASE}/stats`, statsLatency);
    });
  } else {
    // 10% - Health check
    group('health', () => {
      apiRequest('health', `${API_BASE}/health`, healthLatency);
    });
  }

  // Small sleep to simulate realistic user behavior
  sleep(Math.random() * 0.5 + 0.1);  // 100-600ms between requests
}

// Setup function - runs once before test
export function setup() {
  // Verify server is reachable
  const res = http.get(`${API_BASE}/health`);
  if (res.status !== 200) {
    throw new Error(`Server not reachable at ${BASE_URL}. Status: ${res.status}`);
  }

  console.log(`Load test starting against ${BASE_URL}`);
  console.log(`Health check passed. Server version: ${JSON.parse(res.body).version || 'unknown'}`);
  console.log(`Cache bypass: ${NO_CACHE ? 'ENABLED (each request has unique params)' : 'disabled'}`);
  console.log(`Scenario: ${__ENV.SCENARIO || 'constant'}`);

  return { startTime: Date.now() };
}

// Teardown function - runs once after test
export function teardown(data) {
  const duration = (Date.now() - data.startTime) / 1000;
  console.log(`\nLoad test completed in ${duration.toFixed(1)}s`);
}

// Handle summary output
export function handleSummary(data) {
  const summary = {
    'Total Requests': data.metrics.total_requests?.values?.count || 0,
    'Failed Requests': data.metrics.http_req_failed?.values?.rate ?
      `${(data.metrics.http_req_failed.values.rate * 100).toFixed(2)}%` : '0%',
    'Avg Response Time': data.metrics.http_req_duration?.values?.avg ?
      `${data.metrics.http_req_duration.values.avg.toFixed(2)}ms` : 'N/A',
    'p95 Response Time': data.metrics.http_req_duration?.values?.['p(95)'] ?
      `${data.metrics.http_req_duration.values['p(95)'].toFixed(2)}ms` : 'N/A',
    'p99 Response Time': data.metrics.http_req_duration?.values?.['p(99)'] ?
      `${data.metrics.http_req_duration.values['p(99)'].toFixed(2)}ms` : 'N/A',
    'Search p95': data.metrics.search_latency?.values?.['p(95)'] ?
      `${data.metrics.search_latency.values['p(95)'].toFixed(2)}ms` : 'N/A',
  };

  console.log('\n=== Summary ===');
  for (const [key, value] of Object.entries(summary)) {
    console.log(`${key}: ${value}`);
  }

  return {
    stdout: textSummary(data, { indent: ' ', enableColors: true }),
  };
}

// Text summary helper (simplified, k6 has built-in)
function textSummary(data, options) {
  // Return empty - let k6 handle the default summary
  return '';
}
