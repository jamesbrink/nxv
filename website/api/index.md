# HTTP API

nxv includes a REST API server for programmatic access.

## Public Instance

A public instance is available for testing and light usage:

- **Web UI:** [nxv.urandom.io](https://nxv.urandom.io/)
- **API Docs:** [nxv.urandom.io/docs](https://nxv.urandom.io/docs)
- **API Base:** `https://nxv.urandom.io/api/v1`

::: tip Try it now

You can use the public API directly without setting up your own server:

```bash
curl "https://nxv.urandom.io/api/v1/search?q=python&limit=5"
```

:::

## Self-Hosted

To run your own instance, start the server with `nxv serve`.

### Base URL

```
http://localhost:8080/api/v1
```

## Authentication

No authentication required. Rate limiting can be enabled per IP address with
`--rate-limit`.

## Response Wrapper

All responses use a standard wrapper:

```json
{
  "data": [ ... ],
  "meta": {
    "total": 42,
    "limit": 50,
    "offset": 0,
    "has_more": false
  }
}
```

The `meta` field is present for paginated list responses and omitted for
single-item responses.

## Endpoints

### Search Packages

```
GET /api/v1/search
```

**Query Parameters:**

| Parameter    | Type    | Description                                                                  |
| ------------ | ------- | ---------------------------------------------------------------------------- |
| `q`          | string  | Search query (required)                                                      |
| `version`    | string  | Version filter (prefix match)                                                |
| `exact`      | boolean | Exact name match                                                             |
| `all_depths` | boolean | Include every attribute depth; requires `version` and conflicts with `exact` |
| `license`    | string  | License filter                                                               |
| `sort`       | string  | Sort order: relevance (default), date, version, name                         |
| `reverse`    | boolean | Reverse sort                                                                 |
| `limit`      | integer | Max results (default: 50, capped at 100)                                     |
| `offset`     | integer | Results to skip (default: 0)                                                 |

::: tip Description Search

Description search uses a separate endpoint:
`GET /api/v1/search/description?q=<query>&limit=50&offset=0`

:::

**Example:**

```bash
curl "http://localhost:8080/api/v1/search?q=python&version=3.11&limit=5"
```

**Response:**

```json
{
  "data": [
    {
      "id": 1,
      "attribute_path": "python311",
      "name": "python311",
      "version": "3.11.4",
      "description": "A high-level dynamically-typed programming language",
      "license": ["Python-2.0"],
      "homepage": "https://www.python.org",
      "maintainers": ["Fabian Affolter"],
      "platforms": ["x86_64-linux", "aarch64-darwin"],
      "source_path": "pkgs/development/interpreters/python/cpython/default.nix",
      "known_vulnerabilities": null,
      "first_commit_hash": "abc123...",
      "first_commit_date": "2023-06-15T00:00:00Z",
      "last_commit_hash": "def456...",
      "last_commit_date": "2023-12-01T00:00:00Z"
    }
  ],
  "meta": {
    "total": 42,
    "limit": 5,
    "offset": 0,
    "has_more": true
  }
}
```

Version-qualified prefix searches resolve the shallowest matching attribute-path
tier before applying `version`. Set `all_depths=true` to include nested tiers. A
version miss remains a successful HTTP 200 response with an empty `data` array
and an additive explanation in `meta.resolution`:

```json
{
  "data": [],
  "meta": {
    "total": 0,
    "limit": 50,
    "offset": 0,
    "has_more": false,
    "resolution": {
      "scope": "shallowest",
      "resolved_depth": 0,
      "requested_version": "2.7.3",
      "version_matched": false,
      "deeper_matches_available": true,
      "suggestions": [{ "attribute_path": "python", "version": "2.7.12" }]
    }
  }
}
```

`resolution` is omitted for unversioned and description searches. Existing
clients may ignore it; the package-row and pagination shapes are unchanged.

### Get Package Versions

```
GET /api/v1/packages/{attr}
```

Returns all version records for a package.

**Example:**

```bash
curl "http://localhost:8080/api/v1/packages/python311"
```

### Get Specific Version

```
GET /api/v1/packages/{attr}/versions/{version}
GET /api/v1/packages/{attr}/versions/{version}/first
GET /api/v1/packages/{attr}/versions/{version}/last
```

Get the most recent occurrence (the base endpoint behaves the same as `/last`),
the first occurrence, or the last occurrence of a specific version. All three
return a single object, not an array.

**Example:**

```bash
curl "http://localhost:8080/api/v1/packages/python311/versions/3.11.4/first"
```

### Version History

```
GET /api/v1/packages/{attr}/history
```

Returns version history entries with first/last seen dates.

**Example:**

```bash
curl "http://localhost:8080/api/v1/packages/python311/history"
```

**Response:**

```json
{
  "data": [
    {
      "version": "3.11.4",
      "first_seen": "2023-06-15T00:00:00Z",
      "last_seen": "2023-12-01T00:00:00Z",
      "is_insecure": false
    },
    {
      "version": "3.11.3",
      "first_seen": "2023-04-05T00:00:00Z",
      "last_seen": "2023-06-14T00:00:00Z",
      "is_insecure": true,
      "vulnerabilities": "[\"CVE-2023-24329\"]"
    }
  ]
}
```

`is_insecure` means nixpkgs reports a known advisory for that **software
version**, resolved by package name so every attribute packaging the same build
agrees. `vulnerabilities` carries the advisory text and is omitted when the
version is clean. Neither field tells you whether nix will refuse to build a
given attribute at a given revision — for that, read `known_vulnerabilities` from
`/packages/{attr}/versions/{version}/first`.

### Index Statistics

```
GET /api/v1/stats
```

**Response:**

```json
{
  "data": {
    "total_ranges": 1751848,
    "unique_names": 137204,
    "unique_versions": 291563,
    "oldest_commit_date": "2016-09-28T00:00:00Z",
    "newest_commit_date": "2026-06-11T00:00:00Z",
    "last_indexed_commit": "abc123...",
    "last_indexed_date": "2026-06-11T00:00:00Z",
    "channels": [
      {
        "channel": "nixpkgs-unstable",
        "releases_ingested": 4127,
        "releases_pending": 0,
        "releases_failed": 23,
        "releases_skipped": 0,
        "newest_release": "nixpkgs-26.11pre1014844.b503dde36150",
        "newest_release_date": "2026-06-11T00:00:00Z"
      }
    ]
  }
}
```

The `channels` array reports per-channel snapshot ingestion coverage (schema v4
indexes); it is omitted for older indexes.

### Health Check

```
GET /api/v1/health
```

**Response:**

```json
{
  "status": "ok",
  "version": "<nxv version>",
  "index_commit": "abc123..."
}
```

### Metrics

```
GET /api/v1/metrics
```

Returns server, database, rate-limit, runtime, latency, and per-minute activity
metrics. Intended for monitoring dashboards; responses are never cached.

**Example:**

```bash
curl "http://localhost:8080/api/v1/metrics"
```

## Error Responses

All errors return a JSON object:

```json
{
  "code": "NOT_FOUND",
  "message": "Package 'foobar' not found"
}
```

**HTTP Status Codes:**

| Code | Description                                                   |
| ---- | ------------------------------------------------------------- |
| 200  | Success                                                       |
| 400  | Bad request (invalid parameters)                              |
| 404  | Not found                                                     |
| 429  | Rate limited                                                  |
| 500  | Internal server error                                         |
| 503  | Service unavailable (no/corrupt index, or server at capacity) |
| 504  | Gateway timeout (DB operation exceeded `NXV_DB_TIMEOUT_SECS`) |

## OpenAPI Documentation

Interactive API documentation is available at:

```
http://localhost:8080/docs
```

The raw OpenAPI specification is served at `/openapi.json`.

## CORS

Enable CORS for browser access:

```bash
# All origins
nxv serve --cors

# Specific origins
nxv serve --cors-origins "https://example.com,https://app.example.com"
```

## Response Headers

When rate limiting is enabled (`--rate-limit`), the server adds standard
`X-RateLimit-*` headers to responses:

| Header                  | Description                                              |
| ----------------------- | -------------------------------------------------------- |
| `X-RateLimit-Limit`     | The configured per-IP request limit                      |
| `X-RateLimit-Remaining` | Requests remaining in the current window                 |
| `X-RateLimit-After`     | Seconds to wait before retrying (present when limited)   |
| `Retry-After`           | Same value as `X-RateLimit-After` (present when limited) |
