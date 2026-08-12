/* nxv — front-end logic, wired to the live /api/v1 endpoints */
(() => {
  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => [...r.querySelectorAll(s)];

  const API_BASE = '/api/v1';
  const PAGE_SIZE = 50;
  // flake.nix landed 2020-02-10, but v4 rows carry channel-release
  // observation dates that lag commits by hours-to-weeks. 2020-03-26 sits in
  // the gap between the eras: observations after it are guaranteed
  // flake-capable; the legacy form emitted before it works on any tree.
  const FLAKES_EPOCH = new Date('2020-03-26T00:00:00Z');

  // Nix keywords + non-identifier segments must be re-quoted in emitted
  // commands (aspellDicts.or -> aspellDicts."or"), and the ref then needs
  // shell quoting.
  const NIX_KEYWORDS = new Set([
    'or', 'if', 'then', 'else', 'assert', 'with', 'let', 'in', 'rec', 'inherit',
  ]);
  const isPlainNixIdent = (s) => /^[A-Za-z_][A-Za-z0-9_'-]*$/.test(s) && !NIX_KEYWORDS.has(s);
  const attrForCmd = (attr) => {
    let quoted = false;
    const printable = (attr || '')
      .split('.')
      .map((seg) => {
        if (isPlainNixIdent(seg)) return seg;
        quoted = true;
        return `"${seg.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`;
      })
      .join('.');
    return { printable, quoted };
  };
  const flakeRef = (hash, attr) => {
    const { printable, quoted } = attrForCmd(attr);
    const ref = `nixpkgs/${hash}#${printable}`;
    return quoted ? `'${ref}'` : ref;
  };

  const STATE = {
    query: '',
    filters: {
      exact: false,
      allDepths: false,
      version: '',
      arch: '',
      license: '',
      sort: 'relevance',
      includeInsecure: false,
    },
    view: 'rows',
    page: 1,
    pageSize: PAGE_SIZE,
    total: 0,
    hasMore: false,
    lastLatencyMs: null,
    stats: null,
    health: null,
    reqSeq: 0,
    historyCache: new Map(),
    firstRowCache: new Map(),
    renderedRows: [], // cached for instant view-switch on toggle
    resolution: null,
  };

  const els = {};
  const cache = (id) => (els[id] = els[id] || document.getElementById(id));

  // ---------- util ----------
  const fmtDate = (d) => {
    if (!d) return '—';
    try {
      return new Date(d).toLocaleDateString('en-US', {
        year: 'numeric',
        month: 'short',
        day: '2-digit',
      });
    } catch {
      return String(d);
    }
  };
  const fmtNum = (n) => (n == null ? '—' : Number(n).toLocaleString());
  const archLabel = (p) =>
    p
      .replace('x86_64-linux', 'x86_64·linux')
      .replace('aarch64-linux', 'aarch64·linux')
      .replace('x86_64-darwin', 'x86_64·macos')
      .replace('aarch64-darwin', 'aarch64·macos')
      .replace('i686-linux', 'i686·linux')
      .replace('armv7l-linux', 'armv7·linux');
  const predatesFlakes = (d) => (d ? new Date(d) < FLAKES_EPOCH : false);
  const shortHash = (h) => (h || '').slice(0, 7);
  const truncateName = (name) => {
    if (!name) return '';
    // Strip trailing git-rev-style hex suffix (7+ chars), e.g. 'vimpager-a4da4d…'.
    const stripped = name.replace(/-[0-9a-f]{7,}$/i, '');
    if (stripped.length <= 25) return stripped;
    return stripped.slice(0, 25) + '…';
  };
  const escapeHtml = (s) =>
    String(s ?? '').replace(
      /[&<>"']/g,
      (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]
    );

  function parseJsonArrayOrString(s) {
    if (s == null || s === '') return [];
    if (typeof s !== 'string') return Array.isArray(s) ? s : [String(s)];
    const t = s.trim();
    // The DB sometimes stores a literal "null"/"None" sentinel for missing
    // JSON; the server treats those as secure, so we must too.
    if (t === '' || t === 'null' || t === 'None') return [];
    if (t.startsWith('[')) {
      try {
        const v = JSON.parse(t);
        return Array.isArray(v) ? v : [String(v)];
      } catch {
        return [s];
      }
    }
    return [s];
  }

  function toRow(p) {
    const licenses = parseJsonArrayOrString(p.license);
    const platforms = parseJsonArrayOrString(p.platforms).filter(Boolean);
    const vulns = parseJsonArrayOrString(p.known_vulnerabilities);
    return {
      id: p.id,
      name: p.name,
      attr: p.attribute_path,
      ver: p.version,
      desc: p.description || '',
      first: p.first_commit_date,
      last: p.last_commit_date,
      hash: p.first_commit_hash,
      lastHash: p.last_commit_hash,
      platforms,
      license: licenses.join(' / ') || '',
      insecure: vulns.length ? vulns : null,
      legacy: predatesFlakes(p.last_commit_date),
      homepage: p.homepage || null,
      sourcePath: p.source_path || null,
    };
  }

  async function api(path) {
    const t0 = performance.now();
    const res = await fetch(path, { headers: { Accept: 'application/json' } });
    const latency = performance.now() - t0;
    if (!res.ok) {
      const err = new Error(`${res.status} ${res.statusText}`);
      err.status = res.status;
      throw err;
    }
    const json = await res.json();
    return { json, latency };
  }

  // ---------- toast / copy ----------
  function showToast(msg) {
    const t = cache('toast');
    t.textContent = msg;
    t.style.opacity = '1';
    t.style.transform = 'translateY(0)';
    clearTimeout(t._t);
    t._t = setTimeout(() => {
      t.style.opacity = '0';
      t.style.transform = 'translateY(1rem)';
    }, 1800);
  }
  async function copy(text) {
    try {
      await navigator.clipboard.writeText(text);
      showToast(`copied · ${text.length > 56 ? text.slice(0, 56) + '…' : text}`);
    } catch {
      showToast('copy failed');
    }
  }

  function buildFlakeCmd(r) {
    const isLegacy = r.legacy || predatesFlakes(r.last);
    const insecurePrefix = r.insecure ? 'NIXPKGS_ALLOW_INSECURE=1 ' : '';
    const impure = r.insecure ? ' --impure' : '';
    // For the flake path the ref MUST point at a commit that has flake.nix —
    // i.e. post 2020-02-10. first_commit_hash can predate that even for a
    // version whose last-seen commit is current, so prefer last_commit_hash
    // here. For legacy (pre-flake) tarball imports either hash works; keep
    // the historical preference for first_commit_hash.
    // Commands always embed the FULL hash: `nix` resolves github: refs via
    // GitHub's API, which 422s on abbreviated SHAs that are ambiguous in
    // nixpkgs' ~1M-commit history (issue #21).
    const ref = isLegacy ? r.hash || r.lastHash : r.lastHash || r.hash;
    return isLegacy
      ? `${insecurePrefix}nix-shell -p '(import (builtins.fetchTarball "https://github.com/NixOS/nixpkgs/archive/${ref}.tar.gz") {}).${attrForCmd(r.attr).printable}'`
      : `${insecurePrefix}nix shell${impure} ${flakeRef(ref, r.attr)}`;
  }

  // ---------- URL state (refresh-safe, shareable) ----------
  function serializeState() {
    const p = new URLSearchParams();
    if (STATE.query) p.set('q', STATE.query);
    if (STATE.filters.exact) p.set('exact', '1');
    if (STATE.filters.allDepths) p.set('all_depths', '1');
    if (STATE.filters.version) p.set('version', STATE.filters.version);
    if (STATE.filters.arch) p.set('arch', STATE.filters.arch);
    if (STATE.filters.license) p.set('license', STATE.filters.license);
    if (STATE.filters.sort && STATE.filters.sort !== 'relevance') p.set('sort', STATE.filters.sort);
    if (STATE.filters.includeInsecure) p.set('insecure', '1');
    if (STATE.view && STATE.view !== 'rows') p.set('view', STATE.view);
    if (STATE.page > 1) p.set('page', String(STATE.page));
    return p;
  }

  function syncUrl() {
    const qs = serializeState().toString();
    const url = qs ? `${window.location.pathname}?${qs}` : window.location.pathname;
    // replaceState so every keystroke doesn't bloat browser history
    window.history.replaceState(null, '', url);
  }

  function hydrateFromUrl() {
    const p = new URLSearchParams(window.location.search);
    STATE.query = p.get('q') || '';
    STATE.filters.exact = p.get('exact') === '1';
    STATE.filters.allDepths = p.get('all_depths') === '1';
    STATE.filters.version = p.get('version') || '';
    const parsedVersion = parseQuery(STATE.query).ver;
    if (STATE.filters.exact || !(STATE.filters.version || parsedVersion)) {
      STATE.filters.allDepths = false;
    }
    STATE.filters.arch = p.get('arch') || '';
    STATE.filters.license = p.get('license') || '';
    STATE.filters.sort = p.get('sort') || 'relevance';
    STATE.filters.includeInsecure = p.get('insecure') === '1';
    STATE.view = p.get('view') || 'rows';
    const pg = parseInt(p.get('page') || '1', 10);
    STATE.page = Number.isFinite(pg) && pg > 0 ? pg : 1;
  }

  // ---------- query parsing ----------
  function parseQuery(q) {
    const m = q.trim().match(/^(.+?)\s+(v?\d[\d.]*[\w.-]*)$/i);
    if (m && !m[1].match(/\d$/)) return { pkg: m[1].trim(), ver: m[2].replace(/^v/i, '') };
    return { pkg: q.trim(), ver: null };
  }

  // ---------- filter chips ----------
  function cycleFilter(key) {
    const cycles = {
      exact: [false, true],
      allDepths: [false, true],
      version: ['', '2.7', '3.11', '3.12', '18', '22'],
      arch: ['', 'x86_64-linux', 'aarch64-linux', 'x86_64-darwin', 'aarch64-darwin'],
      license: ['', 'MIT', 'GPL-3.0+', 'BSD-3-Clause', 'Apache-2.0', 'LGPL-2.1+'],
      sort: ['relevance', 'date', 'name', 'version'],
      includeInsecure: [false, true],
    };
    const cycle = cycles[key];
    const cur = STATE.filters[key];
    const idx = cycle.indexOf(cur);
    STATE.filters[key] = cycle[(idx + 1) % cycle.length];
  }

  function renderFilterChips() {
    const labels = {
      exact: () => (STATE.filters.exact ? 'on' : 'off'),
      'all-depths': () => (STATE.filters.allDepths ? 'all' : 'closest'),
      version: () => STATE.filters.version || 'any',
      arch: () => (STATE.filters.arch ? archLabel(STATE.filters.arch) : 'any'),
      license: () => STATE.filters.license || 'any',
      sort: () => STATE.filters.sort,
      'include-insecure': () => (STATE.filters.includeInsecure ? 'yes' : 'no'),
    };
    $$('.chip[data-filter]').forEach((el) => {
      const k = el.dataset.filter;
      if (k === 'include-insecure') {
        el.innerHTML = `include insecure: <span class="ml-1 text-[var(--color-fog-0)]">${labels[k]()}</span>`;
        el.classList.toggle('active', STATE.filters.includeInsecure);
      } else {
        const label = el.textContent.split(':')[0].trim();
        el.innerHTML = `${label}: <span class="ml-1 text-[var(--color-fog-0)]">${labels[k]()}</span>`;
        const stateKey = k === 'all-depths' ? 'allDepths' : k;
        const v = STATE.filters[stateKey];
        const isActive = k === 'exact' ? STATE.filters.exact : k === 'all-depths' ? STATE.filters.allDepths : k === 'sort' ? v !== 'relevance' : !!v;
        el.classList.toggle('active', !!isActive);
      }
    });
  }

  // ---------- search ----------
  // The /api/v1/search endpoint paginates server-side but has no params for
  // `arch` or an insecure toggle, so when those client-only filters are
  // active we fetch a large window and paginate the filtered list ourselves.
  // This keeps the count and the prev/next controls consistent with what the
  // user actually sees.
  const CLIENT_FILTER_LIMIT = 500;

  function clientFilterActive() {
    return !!STATE.filters.arch || !STATE.filters.includeInsecure;
  }

  function buildSearchUrl(clientFiltered) {
    const parsed = parseQuery(STATE.query);
    const pkg = parsed.pkg;
    const params = new URLSearchParams();
    // API requires q — use " " as no-op to fetch top rows
    params.set('q', pkg || '');
    const ver = STATE.filters.version || parsed.ver || '';
    if (ver) params.set('version', ver);
    if (STATE.filters.exact) params.set('exact', 'true');
    if (STATE.filters.allDepths && ver) params.set('all_depths', 'true');
    if (STATE.filters.license) params.set('license', STATE.filters.license);
    if (STATE.filters.sort) params.set('sort', STATE.filters.sort);
    if (clientFiltered) {
      params.set('limit', String(CLIENT_FILTER_LIMIT));
      params.set('offset', '0');
    } else {
      params.set('limit', String(STATE.pageSize));
      params.set('offset', String((STATE.page - 1) * STATE.pageSize));
    }
    return `${API_BASE}/search?${params.toString()}`;
  }

  async function runSearch(opts = {}) {
    if (opts.resetPage !== false) STATE.page = 1;
    syncUrl();
    const seq = ++STATE.reqSeq;

    // empty query + no filters → show welcome
    const noQuery = !STATE.query.trim();
    const noFilters = !STATE.filters.version && !STATE.filters.arch && !STATE.filters.license;
    if (noQuery && noFilters) {
      renderWelcome();
      return;
    }

    const clientFiltered = clientFilterActive();
    const url = buildSearchUrl(clientFiltered);
    setResultsStatus('running…', '');
    try {
      const { json, latency } = await api(url);
      if (seq !== STATE.reqSeq) return; // a newer request started — drop this
      STATE.lastLatencyMs = latency;
      const items = (json.data || []).map(toRow);
      const serverMeta = json.meta || { total: items.length, has_more: false };
      STATE.resolution = serverMeta.resolution || null;

      // client-side filters the API doesn't support
      let filtered = items;
      if (STATE.filters.arch)
        filtered = filtered.filter((r) => r.platforms.includes(STATE.filters.arch));
      if (!STATE.filters.includeInsecure) filtered = filtered.filter((r) => !r.insecure);

      let rows,
        total,
        hasMore,
        truncated = false;
      if (clientFiltered) {
        // paginate the filtered list on the client so count + prev/next agree
        total = filtered.length;
        // clamp out-of-range deep links (?page=N beyond the last page) to the
        // last valid page instead of rendering an empty slice
        const totalPages = Math.max(1, Math.ceil(total / STATE.pageSize));
        if (STATE.page > totalPages) {
          STATE.page = totalPages;
          syncUrl();
        }
        const start = (STATE.page - 1) * STATE.pageSize;
        const end = start + STATE.pageSize;
        rows = filtered.slice(start, end);
        hasMore = end < filtered.length;
        // the server capped us at CLIENT_FILTER_LIMIT rows; if it says there's
        // more unfiltered data available, our filtered total is a lower bound
        truncated = !!serverMeta.has_more && items.length >= CLIENT_FILTER_LIMIT;
      } else {
        rows = filtered;
        total = serverMeta.total;
        hasMore = !!serverMeta.has_more;
        // server-side pagination: an out-of-range page came back empty while
        // the total says data exists — clamp to the last valid page and
        // refetch it (converges: the clamped page is always in range)
        const totalPages = Math.max(1, Math.ceil((total || 0) / STATE.pageSize));
        if (STATE.page > totalPages) {
          STATE.page = totalPages;
          runSearch({ resetPage: false });
          return;
        }
      }
      STATE.total = total;
      STATE.hasMore = hasMore;

      render(rows, STATE.resolution);
      setResultsStatus(
        `results / ${fmtNum(total)}${truncated ? '+' : ''}`,
        `${(latency / 1000).toFixed(3)}s · api`
      );
      renderPagination({ total, has_more: hasMore });
    } catch (e) {
      if (seq !== STATE.reqSeq) return;
      renderError(e);
    }
  }

  function setResultsStatus(count, time) {
    cache('resultsCount').textContent = count || '—';
    cache('resultsTime').textContent = time || '—';
  }

  function renderWelcome() {
    STATE.total = 0;
    STATE.hasMore = false;
    STATE.renderedRows = [];
    STATE.resolution = null;
    const w = `
      <div class="px-6 py-16 text-center">
        <div class="mono text-[12px] text-[var(--color-fog-4)] leading-7">
          type a package name above to search<br/>
          <span class="text-[var(--color-fog-3)]">try</span>
          <button class="chip example mx-1" type="button">python 2.7</button>
          <button class="chip example mx-1" type="button">nodejs 18</button>
          <button class="chip example mx-1" type="button">gcc 4.9</button>
          <button class="chip example mx-1" type="button">ffmpeg 5</button>
          <br/>
          <span class="text-[var(--color-fog-3)]">or press</span> <span class="kbd">⌘K</span> <span class="text-[var(--color-fog-3)]">to open the palette</span>
        </div>
      </div>`;
    cache('resultsBody').innerHTML = w;
    cache('resultsCards').innerHTML = '';
    cache('resultsRows').classList.remove('hidden');
    cache('resultsCards').classList.add('hidden');
    setResultsStatus('results / —', '—');
    renderPagination({ total: 0, has_more: false });
    // rewire welcome example chips
    $$('#resultsBody .chip.example').forEach((el) =>
      el.addEventListener('click', () => runExample(el.textContent.trim()))
    );
  }

  function renderError(e) {
    STATE.renderedRows = [];
    const err = `
      <div class="px-6 py-12 text-center">
        <div class="mono text-[12px] text-[var(--color-red-glow)]">error · ${escapeHtml(e?.message || 'request failed')}</div>
        <div class="mt-2 mono text-[11px] text-[var(--color-fog-4)]">check that the API server is reachable.</div>
      </div>`;
    cache('resultsBody').innerHTML = err;
    cache('resultsCards').innerHTML = '';
    cache('resultsRows').classList.remove('hidden');
    cache('resultsCards').classList.add('hidden');
    setResultsStatus('results / —', 'error');
    renderPagination({ total: 0, has_more: false });
  }

  function renderEmptyState(resolution) {
    if (!resolution) {
      return `<div class="px-6 py-16 text-center">
        <div class="mono text-[12px] text-[var(--color-fog-4)]">no results — try loosening filters, or press <span class="kbd">⌘K</span> to browse.</div>
      </div>`;
    }

    const matched = resolution.version_matched;
    const title = matched
      ? 'matching versions were excluded by active filters'
      : `no direct match for version ${escapeHtml(resolution.requested_version || '')}`;
    const suggestions = (resolution.suggestions || [])
      .map(
        (suggestion) =>
          `<button type="button" class="chip" data-suggestion-attr="${escapeHtml(suggestion.attribute_path)}" data-suggestion-version="${escapeHtml(suggestion.version)}">${escapeHtml(suggestion.attribute_path)} ${escapeHtml(suggestion.version)}</button>`
      )
      .join('');
    const deeper = resolution.deeper_matches_available
      ? '<button type="button" class="btn btn-ghost" data-search-all-depths>include deeper matches</button>'
      : '';
    return `<div class="px-6 py-14 text-center">
      <div class="mono text-[12px] text-[var(--color-fog-3)]">${title}</div>
      ${suggestions ? `<div class="mt-4 flex flex-wrap justify-center gap-2">${suggestions}</div>` : ''}
      ${deeper ? `<div class="mt-4">${deeper}</div>` : ''}
    </div>`;
  }

  function bindEmptyStateActions() {
    $$('[data-suggestion-attr]').forEach((el) => {
      el.addEventListener('click', () => {
        STATE.query = el.dataset.suggestionAttr || '';
        STATE.filters.version = el.dataset.suggestionVersion || '';
        STATE.filters.allDepths = false;
        cache('searchInput').value = STATE.query;
        renderFilterChips();
        runSearch();
      });
    });
    $$('[data-search-all-depths]').forEach((el) => {
      el.addEventListener('click', () => {
        STATE.filters.exact = false;
        STATE.filters.allDepths = true;
        renderFilterChips();
        runSearch();
      });
    });
  }

  function render(rows, resolution = STATE.resolution) {
    STATE.renderedRows = rows;
    const isCards = STATE.view === 'cards';
    if (!rows.length) {
      cache('resultsBody').innerHTML = renderEmptyState(resolution);
      cache('resultsCards').innerHTML = '';
      cache('resultsRows').classList.remove('hidden');
      cache('resultsCards').classList.add('hidden');
      bindEmptyStateActions();
      return;
    }
    cache('resultsRows').classList.toggle('hidden', isCards);
    cache('resultsCards').classList.toggle('hidden', !isCards);
    if (isCards) {
      const body = cache('resultsCards');
      body.classList.add('cards-grid');
      body.innerHTML = rows.map((r, i) => renderCard(r, i)).join('');
      bindCardEvents(rows);
    } else {
      const body = cache('resultsBody');
      body.innerHTML = rows.map((r, i) => renderRow(r, i)).join('');
      bindRowEvents(rows);
    }
  }

  function bindRowEvents(rows) {
    const rowByAttrVer = new Map(rows.map((r) => [`${r.attr}::${r.ver}`, r]));
    $$('#resultsBody [data-action]').forEach((el) => {
      el.addEventListener('click', (ev) => {
        ev.stopPropagation();
        const { action } = el.dataset;
        const key = el.dataset.key;
        const r = rowByAttrVer.get(key);
        if (!r) return;
        if (action === 'copy-flake') copy(buildFlakeCmd(r));
        else if (action === 'copy-run')
          copy(`nix run ${flakeRef(r.lastHash || r.hash, r.attr)}`);
        else if (action === 'history') openDrawer(r);
      });
    });
    $$('#resultsBody [data-row]').forEach((el) => {
      el.addEventListener('click', () => {
        const r = rowByAttrVer.get(el.dataset.row);
        if (r) openDrawer(r);
      });
    });
  }

  function bindCardEvents(rows) {
    const cardByAttrVer = new Map(rows.map((r) => [`${r.attr}::${r.ver}`, r]));
    $$('#resultsCards [data-action]').forEach((el) => {
      el.addEventListener('click', (ev) => {
        ev.stopPropagation();
        const { action } = el.dataset;
        const key = el.dataset.key;
        const r = cardByAttrVer.get(key);
        if (!r) return;
        if (action === 'copy-flake') copy(buildFlakeCmd(r));
        else if (action === 'copy-run')
          copy(`nix run ${flakeRef(r.lastHash || r.hash, r.attr)}`);
        else if (action === 'history') openDrawer(r);
      });
    });
    $$('#resultsCards [data-row]').forEach((el) => {
      el.addEventListener('click', () => {
        const r = cardByAttrVer.get(el.dataset.row);
        if (r) openDrawer(r);
      });
    });
  }

  function renderCard(r, i) {
    const flags = [];
    if (r.insecure) {
      const title = escapeHtml(r.insecure.join(' · '));
      flags.push(
        `<span class="chip danger" title="${title}"><svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><path d="M12 8v4M12 16h.01"/></svg>insecure</span>`
      );
    }
    if (r.legacy) flags.push(`<span class="chip warn">pre-flakes</span>`);
    const platformsHtml = r.platforms
      .filter((p) => /^(x86_64|aarch64|i686|armv7l|armv6l)-(linux|darwin)$/.test(p))
      .slice(0, 4)
      .map((p) => {
        const active = STATE.filters.arch === p;
        return `<span class="chip${active ? ' active' : ''}">${archLabel(p)}</span>`;
      })
      .join('');
    const key = `${r.attr}::${r.ver}`;
    const nameFull = r.name || r.attr || '';
    const nameDisplay = truncateName(nameFull) || nameFull;
    const nameHtml = escapeHtml(nameDisplay);
    const nameTitleAttr = nameDisplay === nameFull ? '' : ` title="${escapeHtml(nameFull)}"`;
    const attrHtml = escapeHtml(r.attr);
    const verFull = r.ver || '';
    const verShort = verFull.length > 18 ? verFull.slice(0, 10) + '…' : verFull;
    const verHtml = escapeHtml(verShort);
    const verTitleAttr = verShort === verFull ? '' : ` title="${escapeHtml(verFull)}"`;
    const descHtml = escapeHtml(r.desc);
    const licenseHtml = escapeHtml(r.license || '—');
    const verToneClass = r.insecure
      ? 'card-ver--danger'
      : r.legacy
        ? 'card-ver--warn'
        : 'card-ver--brand';
    const firstSeen = r.first ? fmtDate(r.first) : '—';
    const lastSeen = r.last ? fmtDate(r.last) : '—';
    return `
      <article data-row="${escapeHtml(key)}" class="card group anim-in" style="animation-delay:${i * 18}ms" tabindex="0">
        <header class="card-head">
          <div class="card-head__title min-w-0">
            <h3 class="card-name mono"${nameTitleAttr}>${nameHtml}</h3>
            <p class="card-attr mono"><span class="card-attr__sigil">›</span>${attrHtml}</p>
          </div>
          <span class="card-ver mono ${verToneClass} tabular-nums"${verTitleAttr}>${verHtml}</span>
        </header>
        <p class="card-desc">${descHtml || '<span class="text-[var(--color-fog-4)]">—</span>'}</p>
        <div class="card-chips">
          ${flags.join('')}${platformsHtml}
          <span class="chip">${licenseHtml}</span>
        </div>
        <dl class="card-meta mono">
          <div><dt>first</dt><dd class="tabular-nums">${firstSeen}</dd></div>
          <div><dt>last</dt><dd class="tabular-nums">${lastSeen}</dd></div>
          <div><dt>rev</dt><dd class="tabular-nums">#${shortHash(r.hash || r.lastHash)}</dd></div>
        </dl>
        <footer class="card-actions">
          <button class="card-action" data-action="copy-flake" data-key="${escapeHtml(key)}" title="copy flake ref" aria-label="copy flake ref">
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
            <span>flake</span>
          </button>
          <button class="card-action" data-action="copy-run" data-key="${escapeHtml(key)}" title="copy nix run ref" aria-label="copy nix run ref">
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polygon points="5 3 19 12 5 21 5 3"/></svg>
            <span>run</span>
          </button>
          <button class="card-action" data-action="history" data-key="${escapeHtml(key)}" title="version history" aria-label="version history">
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/></svg>
            <span>history</span>
          </button>
        </footer>
      </article>`;
  }

  function refreshCurrentView() {
    if (STATE.renderedRows.length) render(STATE.renderedRows);
  }

  function renderRow(r, i) {
    const isLegacy = r.legacy;
    const flags = [];
    if (r.insecure) {
      const title = escapeHtml(r.insecure.join(' · '));
      flags.push(
        `<span class="chip danger" title="${title}"><svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><path d="M12 8v4M12 16h.01"/></svg>insecure</span>`
      );
    }
    if (isLegacy) flags.push(`<span class="chip warn">pre-flakes</span>`);

    const platformsHtml = r.platforms
      .filter((p) => /^(x86_64|aarch64|i686|armv7l|armv6l)-(linux|darwin)$/.test(p))
      .slice(0, 4)
      .map((p) => {
        const active = STATE.filters.arch === p;
        return `<span class="chip${active ? ' active' : ''}" style="font-size:10px; padding: 1px 6px;">${archLabel(p)}</span>`;
      })
      .join('');

    const key = `${r.attr}::${r.ver}`;
    const nameHtml = escapeHtml(truncateName(r.name));
    const attrHtml = escapeHtml(r.attr);
    const licenseHtml = escapeHtml(r.license || '—');
    const descHtml = escapeHtml(r.desc);
    // Truncate long versions (like full git hashes) to fit the column;
    // keep the full string in title= so users can hover to see it.
    const verFull = r.ver || '';
    const verShort = verFull.length > 20 ? verFull.slice(0, 10) + '…' : verFull;
    const verHtml = escapeHtml(verShort);
    const verTitleAttr = verShort === verFull ? '' : ` title="${escapeHtml(verFull)}"`;
    const verToneClass = r.insecure
      ? 'card-ver--danger'
      : isLegacy
        ? 'card-ver--warn'
        : 'card-ver--brand';

    return `
      <div data-row="${escapeHtml(key)}" class="group grid grid-cols-[minmax(180px,1.6fr)_100px_minmax(200px,2fr)_100px_150px] lg:grid-cols-[minmax(180px,1.6fr)_100px_minmax(200px,2fr)_100px_100px_150px] gap-4 items-center px-5 py-3.5 cursor-pointer transition-colors anim-in hover:bg-[var(--color-ink-1)]" style="animation-delay:${i * 12}ms; border-bottom: 1px solid var(--border-subtle);">
        <div class="min-w-0">
          <div class="flex items-baseline gap-2 min-w-0">
            <span class="mono text-[13px] text-[var(--color-fog-0)] font-medium truncate">${nameHtml}</span>
            <span class="mono text-[11px] text-[var(--color-nix-400)] truncate">${attrHtml}</span>
          </div>
          <div class="mono text-[11px] text-[var(--color-fog-4)] mt-1 flex items-center gap-1.5 min-w-0">
            <span class="truncate" style="max-width: 180px;">${licenseHtml}</span>
            <span class="text-[var(--color-ink-5)]" aria-hidden="true">·</span>
            <span class="mono">#${shortHash(r.hash || r.lastHash)}</span>
          </div>
        </div>
        <div class="min-w-0">
          <span class="ver-badge ${verToneClass} tabular-nums"${verTitleAttr}>${verHtml}</span>
        </div>
        <div class="hidden md:block min-w-0">
          <div class="text-[13px] text-[var(--color-fog-2)] truncate">${descHtml || '<span class="text-[var(--color-fog-4)]">—</span>'}</div>
          <div class="mt-1.5 flex flex-wrap gap-1">
            ${flags.join('')}${platformsHtml}
          </div>
        </div>
        <div class="hidden lg:block mono text-[12px] text-[var(--color-fog-3)] tabular-nums">${fmtDate(r.first)}</div>
        <div class="mono text-[12px] text-[var(--color-fog-3)] tabular-nums">${fmtDate(r.last)}</div>
        <div class="flex items-center justify-end gap-1.5">
          <button class="btn btn-ghost" data-action="copy-flake" data-key="${escapeHtml(key)}" title="copy flake ref">
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
            copy
          </button>
          <button class="btn btn-ghost" data-action="history" data-key="${escapeHtml(key)}" title="version history">
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/></svg>
            log
          </button>
        </div>
      </div>
    `;
  }

  // ---------- pagination ----------
  function renderPagination(meta) {
    const el = cache('pagination');
    const total = meta.total || 0;
    const totalPages = Math.max(1, Math.ceil(total / STATE.pageSize));
    // defensive display clamp — STATE.page is clamped when responses arrive,
    // but never show a range like "51–7 of 7" even if state drifts
    const page = Math.min(Math.max(1, STATE.page), totalPages);
    const startIdx = total === 0 ? 0 : (page - 1) * STATE.pageSize + 1;
    const endIdx = Math.min(total, page * STATE.pageSize);
    el.innerHTML = `
      <span>page <span class="text-[var(--color-fog-0)]">${page}</span> / <span class="text-[var(--color-fog-0)]">${totalPages}</span> · showing <span class="text-[var(--color-fog-0)]">${startIdx}–${endIdx}</span> of <span class="text-[var(--color-fog-0)]">${fmtNum(total)}</span></span>
      <div class="flex items-center gap-1.5">
        <button class="btn btn-ghost" data-page="prev" ${STATE.page <= 1 ? 'disabled style="opacity:.4; cursor:not-allowed;"' : ''}>← prev</button>
        <button class="btn btn-ghost" data-page="next" ${!meta.has_more ? 'disabled style="opacity:.4; cursor:not-allowed;"' : ''}>next →</button>
      </div>`;
    $$('#pagination [data-page]').forEach((b) => {
      b.addEventListener('click', () => {
        if (b.hasAttribute('disabled')) return;
        if (b.dataset.page === 'prev' && STATE.page > 1) STATE.page -= 1;
        else if (b.dataset.page === 'next' && STATE.hasMore) STATE.page += 1;
        else return;
        runSearch({ resetPage: false });
        window.scrollTo({ top: cache('resultsSection').offsetTop - 80, behavior: 'smooth' });
      });
    });
  }

  // ---------- drawer / version history ----------
  // Focus is returned to the invoking element on close (a11y); falls back to
  // the search input when the invoker is gone or was never focusable.
  let drawerReturnFocus = null;
  const restoreFocus = (stored) => {
    const target =
      stored && stored !== document.body && document.contains(stored)
        ? stored
        : cache('searchInput');
    if (target && typeof target.focus === 'function') target.focus();
  };

  async function openDrawer(r) {
    const drawer = cache('drawerOverlay');
    drawerReturnFocus = document.activeElement;
    drawer.classList.remove('hidden');
    drawer.classList.add('flex');
    drawer.setAttribute('aria-hidden', 'false');
    cache('drawerTitle').textContent = `${r.name} · ${r.attr}`;
    cache('drawerSub').innerHTML =
      `${escapeHtml(r.desc || '—')} <span class="text-[var(--color-ink-4)]">│</span> ${escapeHtml(r.license || '—')}`;
    cache('drawerCount').textContent = '…';
    cache('drawerList').innerHTML =
      `<li class="px-3 py-4 mono text-[12px] text-[var(--color-fog-4)]">loading version history…</li>`;
    cache('timelineViz').innerHTML = '';

    try {
      const versions = await fetchHistory(r.attr);
      cache('drawerCount').textContent = `(${versions.length})`;
      drawTimeline(versions);
      cache('drawerList').innerHTML = versions
        .map((v, idx) => {
          const legacy = predatesFlakes(v.last_seen);
          const tag = v.is_insecure
            ? ' text-[var(--color-red-glow)]'
            : legacy
              ? ' text-[var(--color-amber-glow)]'
              : ' text-[var(--color-fog-0)]';
          const vulns = parseJsonArrayOrString(v.vulnerabilities);
          const vulnTitle = escapeHtml(
            vulns.length ? vulns.join(' · ') : 'known vulnerabilities'
          );
          return `
            <li class="grid grid-cols-[minmax(90px,auto)_1fr_auto] items-center gap-4 px-3 py-2 rounded-[7px] hover:bg-[var(--color-ink-2)] transition">
              <span class="mono text-[12.5px]${tag} tabular-nums">${escapeHtml(v.version)}</span>
              <span class="mono text-[11px] text-[var(--color-fog-3)] tabular-nums">${fmtDate(v.first_seen)}<span class="text-[var(--color-ink-4)] mx-2">→</span>${fmtDate(v.last_seen)}</span>
              <span class="flex items-center gap-2">
                ${v.is_insecure ? `<span class="chip danger" style="font-size:10px; padding:1px 5px;" title="${vulnTitle}">insecure</span>` : ''}
                ${legacy ? '<span class="chip warn" style="font-size:10px; padding:1px 5px;">pre-flakes</span>' : ''}
                <button class="btn btn-ghost" data-history-copy="${idx}" title="copy flake ref">copy</button>
              </span>
            </li>`;
        })
        .join('');

      $$('#drawerList [data-history-copy]').forEach((b) => {
        b.addEventListener('click', async () => {
          const v = versions[parseInt(b.dataset.historyCopy, 10)];
          if (!v) return;
          b.textContent = '…';
          try {
            const row = await fetchFirstOccurrence(r.attr, v.version);
            const rowVulns = parseJsonArrayOrString(row.known_vulnerabilities);
            const synth = {
              attr: r.attr,
              hash: row.first_commit_hash || '',
              // buildFlakeCmd prefers lastHash: first_commit_hash can predate
              // flakes even when the version's last-seen commit does not
              // (issue #21). The row carries it, so pass it through.
              lastHash: row.last_commit_hash || '',
              last: v.last_seen,
              insecure: rowVulns.length ? rowVulns : null,
              legacy: predatesFlakes(v.last_seen),
            };
            copy(buildFlakeCmd(synth));
          } catch (e) {
            showToast(`error · ${e.message}`);
          } finally {
            b.textContent = 'copy';
          }
        });
      });
    } catch (e) {
      cache('drawerCount').textContent = '';
      cache('drawerList').innerHTML = `
        <li class="px-3 py-4 mono text-[12px] text-[var(--color-red-glow)]">error · ${escapeHtml(e.message)}</li>`;
    }
  }

  function closeDrawer() {
    const drawer = cache('drawerOverlay');
    if (drawer.classList.contains('hidden')) return; // already closed (e.g. stray Escape)
    drawer.classList.add('hidden');
    drawer.classList.remove('flex');
    drawer.setAttribute('aria-hidden', 'true');
    restoreFocus(drawerReturnFocus);
    drawerReturnFocus = null;
  }

  async function fetchHistory(attr) {
    if (STATE.historyCache.has(attr)) return STATE.historyCache.get(attr);
    const { json } = await api(
      `${API_BASE}/packages/${encodeURIComponent(attr)}/history?limit=100`
    );
    const versions = (json.data || []).slice();
    STATE.historyCache.set(attr, versions);
    return versions;
  }

  // Returns the first-occurrence row, not just its hash: the copied flake
  // command needs this row's own known_vulnerabilities to decide on
  // NIXPKGS_ALLOW_INSECURE. The history entry's `is_insecure` is a broader
  // question (does this software version have an advisory at all), so it is the
  // wrong input for a command that has to match what nix does at that revision.
  async function fetchFirstOccurrence(attr, version) {
    const key = `${attr}::${version}`;
    if (STATE.firstRowCache.has(key)) return STATE.firstRowCache.get(key);
    const { json } = await api(
      `${API_BASE}/packages/${encodeURIComponent(attr)}/versions/${encodeURIComponent(version)}/first`
    );
    const row = json?.data || {};
    STATE.firstRowCache.set(key, row);
    return row;
  }

  function drawTimeline(history) {
    const el = cache('timelineViz');
    el.innerHTML = '';

    // Derive axis from the actual history, not a hard-coded 2017 floor.
    const times = [];
    for (const v of history) {
      const f = new Date(v.first_seen).getTime();
      const l = new Date(v.last_seen).getTime();
      if (Number.isFinite(f)) times.push(f);
      if (Number.isFinite(l)) times.push(l);
    }
    if (!times.length) return;

    const rawStart = Math.min(...times);
    const rawEnd = Math.max(...times);
    const minSpan = 90 * 24 * 3600e3; // keep a 3-month floor so single-version histories stay legible
    const usableSpan = Math.max(rawEnd - rawStart, minSpan);
    const pad = Math.max(14 * 24 * 3600e3, usableSpan * 0.04);
    const axisStart = rawStart - pad;
    const axisEnd = rawEnd + pad;
    const axisSpan = axisEnd - axisStart;

    const startYear = new Date(axisStart).getUTCFullYear();
    const endYear = new Date(axisEnd).getUTCFullYear();

    const labelEl = document.getElementById('timelineLabel');
    if (labelEl) labelEl.textContent = `timeline · ${startYear} → ${endYear}`;

    const ticksEl = document.getElementById('timelineTicks');
    if (ticksEl) {
      const years = [];
      for (let y = startYear; y <= endYear; y++) years.push(y);
      // thin ticks to keep the row readable on long histories
      const maxTicks = 10;
      const stride = Math.max(1, Math.ceil(years.length / maxTicks));
      const shown = years.filter((_, i) => i % stride === 0);
      if (shown[shown.length - 1] !== years[years.length - 1]) shown.push(years[years.length - 1]);
      ticksEl.innerHTML = shown.map((y) => `<span>'${String(y).slice(2)}</span>`).join('');
    }

    const rows = history.slice(0, 12);
    const rowH = 10;
    const gap = 2;
    const totalH = Math.max(rows.length * (rowH + gap), 100);
    const svgNS = 'http://www.w3.org/2000/svg';
    const svg = document.createElementNS(svgNS, 'svg');
    svg.setAttribute('viewBox', `0 0 1000 ${totalH}`);
    svg.setAttribute('preserveAspectRatio', 'none');
    svg.style.width = '100%';
    svg.style.height = `${totalH}px`;

    for (let y = startYear; y <= endYear; y++) {
      const x = ((new Date(Date.UTC(y, 0, 1)).getTime() - axisStart) / axisSpan) * 1000;
      if (x < 0 || x > 1000) continue;
      const line = document.createElementNS(svgNS, 'line');
      line.setAttribute('x1', x);
      line.setAttribute('x2', x);
      line.setAttribute('y1', 0);
      line.setAttribute('y2', totalH);
      line.setAttribute('stroke', 'var(--color-ink-3)');
      line.setAttribute('stroke-dasharray', '2 4');
      line.setAttribute('stroke-width', '1');
      svg.appendChild(line);
    }

    const flakeT = FLAKES_EPOCH.getTime();
    if (flakeT >= axisStart && flakeT <= axisEnd) {
      const flakeX = ((flakeT - axisStart) / axisSpan) * 1000;
      const flakeLine = document.createElementNS(svgNS, 'line');
      flakeLine.setAttribute('x1', flakeX);
      flakeLine.setAttribute('x2', flakeX);
      flakeLine.setAttribute('y1', 0);
      flakeLine.setAttribute('y2', totalH);
      flakeLine.setAttribute('stroke', 'var(--color-amber-glow)');
      flakeLine.setAttribute('stroke-dasharray', '3 3');
      flakeLine.setAttribute('stroke-width', '1');
      flakeLine.setAttribute('opacity', '0.5');
      svg.appendChild(flakeLine);
    }

    rows.forEach((v, i) => {
      const x1 = Math.max(0, ((new Date(v.first_seen).getTime() - axisStart) / axisSpan) * 1000);
      const x2 = Math.min(1000, ((new Date(v.last_seen).getTime() - axisStart) / axisSpan) * 1000);
      const w = Math.max(2, x2 - x1);
      const y = i * (rowH + gap);
      const rect = document.createElementNS(svgNS, 'rect');
      rect.setAttribute('x', x1);
      rect.setAttribute('y', y);
      rect.setAttribute('width', w);
      rect.setAttribute('height', rowH);
      rect.setAttribute('rx', '1');
      rect.setAttribute('fill', v.is_insecure ? 'var(--color-red-glow)' : 'var(--color-nix-500)');
      rect.setAttribute('opacity', v.is_insecure ? '0.75' : '0.85');
      svg.appendChild(rect);
      if (w > 30) {
        const label = document.createElementNS(svgNS, 'text');
        label.setAttribute('x', Math.min(980, x2 + 4));
        label.setAttribute('y', y + rowH - 1.5);
        label.setAttribute('font-family', 'JetBrains Mono');
        label.setAttribute('font-size', '8.5');
        label.setAttribute('fill', 'var(--color-fog-3)');
        label.textContent = v.version;
        svg.appendChild(label);
      }
    });

    el.appendChild(svg);
  }

  // ---------- stats / header populate ----------
  async function loadBoot() {
    const healthP = api(`${API_BASE}/health`)
      .then(({ json }) => {
        STATE.health = json;
      })
      .catch(() => {});
    const statsP = api(`${API_BASE}/stats`)
      .then(({ json }) => {
        STATE.stats = json.data || json;
      })
      .catch(() => {});
    await Promise.all([healthP, statsP]);

    await refreshMetrics();
    renderHeaderStrip();
    renderHeroStats();
    renderSelfhostCard();

    // keep metrics live — cheap in-memory query, no db work
    setInterval(refreshMetrics, 30_000);
  }

  async function refreshMetrics() {
    try {
      const { json } = await api(`${API_BASE}/metrics`);
      STATE.metrics = json;
      renderActivityCard(json);
      renderLatencyCard(json);
      refreshHeaderLatency(json);
    } catch {
      // leave previous values in place if a poll fails
    }
  }

  function renderHeaderStrip() {
    const stats = STATE.stats;
    const health = STATE.health;

    const operational = !!health;
    const pill = document.getElementById('statusPill');
    if (pill) {
      const dotStyle = operational
        ? ''
        : ' style="background: var(--color-red-glow); box-shadow: 0 0 7px oklch(0.66 0.19 25 / 0.7);"';
      pill.innerHTML = `<span class="pill-dot"${dotStyle}></span><span id="headerStatus">${operational ? 'api operational' : 'api unreachable'}</span><span id="headerLatency" class="hidden lg:inline"></span>`;
    }

    const lastDate = stats?.last_indexed_date ? fmtDate(stats.last_indexed_date) : '—';
    const commit = health?.index_commit || stats?.last_indexed_commit || '';

    const strip = document.getElementById('statusStrip');
    if (strip) {
      const oldest = stats?.oldest_commit_date
        ? new Date(stats.oldest_commit_date).toISOString().slice(0, 7)
        : '—';
      const newest = stats?.newest_commit_date
        ? new Date(stats.newest_commit_date).toISOString().slice(0, 7)
        : '—';
      strip.innerHTML = `
        <span>index · <span class="text-[var(--color-fog-2)]">${lastDate}</span>${commit ? ` · commit <span class="text-[var(--color-fog-2)]">${shortHash(commit)}</span>` : ''}</span>
        <span class="text-[var(--color-ink-5)]" aria-hidden="true">│</span>
        <span>nixpkgs · <span class="text-[var(--color-fog-2)]">${oldest} → ${newest}</span></span>
        <span class="flex-1"></span>
        <span>press <span class="kbd">/</span> to focus</span>`;
    }

    // condensed single-line variant for < md viewports — api status + index
    // date at minimum, so phones aren't blind to telemetry
    const mobileStrip = document.getElementById('statusStripMobile');
    if (mobileStrip) {
      const dotClass = operational
        ? 'text-[var(--color-green-glow)]'
        : 'text-[var(--color-red-glow)]';
      mobileStrip.innerHTML = `
        <span><span class="${dotClass}" aria-hidden="true">●</span> ${operational ? 'api operational' : 'api unreachable'}</span>
        <span>index · <span class="text-[var(--color-fog-3)]">${lastDate}</span></span>${commit ? `
        <span>commit · <span class="text-[var(--color-fog-3)]">${shortHash(commit)}</span></span>` : ''}`;
    }

    // version tag in the hero eyebrow line
    const heroVer = document.getElementById('heroEyebrow');
    if (heroVer && health?.version) {
      heroVer.innerHTML = `// nix version index &nbsp;·&nbsp; v${escapeHtml(health.version)}`;
    }
  }

  function refreshHeaderLatency(metrics) {
    const status = document.getElementById('headerStatus');
    const latency = document.getElementById('headerLatency');
    const p50 = metrics?.latency?.p50_ms;
    if (!status || p50 == null) return;
    status.textContent = STATE.health ? 'api operational' : 'api unreachable';
    // Latency lives in its own lg-only span so the pill stays short at
    // tablet widths where the header would otherwise overflow.
    if (latency) latency.textContent = metrics.latency.samples > 0 ? ` · p50 ${p50.toFixed(1)}ms` : '';
  }

  function renderHeroStats() {
    const s = STATE.stats;
    const wrap = document.getElementById('heroMetrics');
    if (!wrap || !s) return;
    const oldest = s.oldest_commit_date ? new Date(s.oldest_commit_date) : null;
    const newest = s.newest_commit_date ? new Date(s.newest_commit_date) : null;
    const years =
      oldest && newest ? Math.max(1, Math.round((newest - oldest) / (365.25 * 24 * 3600e3))) : null;
    const histTxt = years != null ? `${years}+ yrs` : '—';
    const tile = (value, label) => `
      <div class="panel panel--rail metric">
        <div class="metric-value mono tabular-nums">${value}</div>
        <div class="metric-label mono">${label}</div>
      </div>`;
    wrap.innerHTML = [
      tile(fmtNum(s.unique_names), 'packages'),
      tile(fmtNum(s.unique_versions), 'versions'),
      tile(fmtNum(s.total_ranges), 'records'),
      tile(s.last_indexed_commit ? '#' + shortHash(s.last_indexed_commit) : '—', 'latest'),
      tile(histTxt, 'of history'),
    ].join('');
  }

  function renderLatencyCard(metrics) {
    const main = document.getElementById('latencyMain');
    const lat = metrics?.latency;
    if (main) {
      if (!lat || lat.samples === 0) {
        main.innerHTML = `<span class="text-[var(--color-fog-4)] text-[18px]">waiting for samples…</span>`;
      } else {
        const fmt = (v) => (v < 10 ? v.toFixed(1) : Math.round(v));
        main.innerHTML = `
          ${fmt(lat.p50_ms)}<span class="text-[var(--color-fog-4)] text-[15px]">ms</span>
          <span class="mx-2 text-[var(--color-ink-5)] text-[15px]">/</span>
          ${fmt(lat.p95_ms)}<span class="text-[var(--color-fog-4)] text-[15px]">ms</span>
          <span class="mx-2 text-[var(--color-ink-5)] text-[15px]">/</span>
          ${fmt(lat.p99_ms)}<span class="text-[var(--color-fog-4)] text-[15px]">ms</span>`;
      }
    }
    const sub = document.getElementById('latencySub');
    if (sub) {
      const s = STATE.stats;
      const sampleTxt =
        lat?.samples > 0
          ? `from <span class="text-[var(--color-fog-0)]">${fmtNum(lat.samples)}</span> recent api requests`
          : `awaiting traffic`;
      sub.innerHTML = `
        records indexed · <span class="text-[var(--color-fog-0)]">${fmtNum(s?.total_ranges)}</span><br/>
        ${sampleTxt}`;
    }
  }

  function renderActivityCard(metrics) {
    const buckets = metrics?.activity || [];
    const counts = buckets.map((b) => b.count || 0);
    const max = Math.max(1, ...counts);
    const bars = counts
      .map((c) => {
        const h = c === 0 ? 3 : 6 + (c / max) * 48;
        const idle = c === 0;
        return `<span class="inline-block rounded-[1px] transition" style="width:6px; height:${h.toFixed(1)}px; background:${idle ? 'var(--color-ink-4)' : 'var(--color-green-glow)'}; opacity:${idle ? 0.5 : (0.55 + (h / 54) * 0.4).toFixed(2)};" title="${c} req${c === 1 ? '' : 's'}"></span>`;
      })
      .join('');
    cache('activityBars').innerHTML = bars;

    const summary = document.getElementById('activitySummary');
    if (summary) {
      const total = metrics?.runtime?.total_requests ?? 0;
      const uptime = fmtDuration(metrics?.runtime?.uptime_seconds ?? 0);
      summary.innerHTML = `uptime <span class="text-[var(--color-fog-0)]">${uptime}</span> · <span class="text-[var(--color-fog-0)]">${fmtNum(total)}</span> req${total === 1 ? '' : 's'}`;
    }
  }

  function fmtDuration(totalSeconds) {
    if (!Number.isFinite(totalSeconds) || totalSeconds < 0) return '—';
    const s = Math.floor(totalSeconds);
    if (s < 60) return `${s}s`;
    const m = Math.floor(s / 60);
    if (m < 60) return `${m}m ${s % 60}s`;
    const h = Math.floor(m / 60);
    if (h < 24) return `${h}h ${m % 60}m`;
    const d = Math.floor(h / 24);
    return `${d}d ${h % 24}h`;
  }

  function renderSelfhostCard() {
    const link = document.getElementById('selfhostLink');
    if (link) {
      link.href = 'https://utensils.io/nxv/';
      link.target = '_blank';
      link.rel = 'noopener';
    }
  }

  // ---------- command palette ----------
  const PALETTE_ITEMS = [
    { cat: 'jump', label: 'python', hint: 'popular package', action: () => runExample('python') },
    { cat: 'jump', label: 'nodejs', hint: 'popular package', action: () => runExample('nodejs') },
    { cat: 'jump', label: 'ruby', hint: 'popular package', action: () => runExample('ruby') },
    { cat: 'jump', label: 'gcc', hint: 'toolchain', action: () => runExample('gcc') },
    { cat: 'jump', label: 'postgresql', hint: 'databases', action: () => runExample('postgresql') },
    { cat: 'jump', label: 'ffmpeg', hint: 'media', action: () => runExample('ffmpeg') },
    {
      cat: 'cmd',
      label: 'toggle exact match',
      hint: 'filter',
      action: () => {
        cycleFilter('exact');
        if (STATE.filters.exact) STATE.filters.allDepths = false;
        renderFilterChips();
        runSearch();
      },
    },
    {
      cat: 'cmd',
      label: 'search all attribute depths',
      hint: 'version filter',
      action: () => {
        const parsed = parseQuery(STATE.query);
        if (!(STATE.filters.version || parsed.ver)) {
          showToast('all-depth search requires a version');
          return;
        }
        STATE.filters.exact = false;
        STATE.filters.allDepths = true;
        renderFilterChips();
        runSearch();
      },
    },
    {
      cat: 'cmd',
      label: 'include insecure packages',
      hint: 'filter',
      action: () => {
        cycleFilter('includeInsecure');
        renderFilterChips();
        runSearch();
      },
    },
    {
      cat: 'cmd',
      label: 'sort by relevance',
      hint: 'sort',
      action: () => {
        STATE.filters.sort = 'relevance';
        renderFilterChips();
        runSearch();
      },
    },
    {
      cat: 'cmd',
      label: 'sort by name',
      hint: 'sort',
      action: () => {
        STATE.filters.sort = 'name';
        renderFilterChips();
        runSearch();
      },
    },
    {
      cat: 'cmd',
      label: 'sort by date',
      hint: 'sort',
      action: () => {
        STATE.filters.sort = 'date';
        renderFilterChips();
        runSearch();
      },
    },
    {
      cat: 'cmd',
      label: 'sort by version',
      hint: 'sort',
      action: () => {
        STATE.filters.sort = 'version';
        renderFilterChips();
        runSearch();
      },
    },
    {
      cat: 'cmd',
      label: 'clear filters',
      hint: 'reset',
      action: () => {
        STATE.filters = {
          exact: false,
          allDepths: false,
          version: '',
          arch: '',
          license: '',
          sort: 'relevance',
          includeInsecure: false,
        };
        renderFilterChips();
        runSearch();
      },
    },
    {
      cat: 'go',
      label: 'open api docs',
      hint: '/docs',
      action: () => {
        window.location.href = '/docs';
      },
    },
    {
      cat: 'go',
      label: 'openapi spec',
      hint: '/openapi.json',
      action: () => {
        window.open('/openapi.json', '_blank');
      },
    },
    {
      cat: 'go',
      label: 'github',
      hint: 'source',
      action: () => window.open('https://github.com/utensils/nxv', '_blank'),
    },
    {
      cat: 'go',
      label: 'install guide',
      hint: 'docs',
      action: () => window.open('https://utensils.io/nxv/', '_blank'),
    },
  ];
  let paletteIndex = 0;
  let paletteItems = [];

  let paletteReturnFocus = null;
  function openPalette() {
    const p = cache('palette');
    paletteReturnFocus = document.activeElement;
    p.classList.remove('hidden');
    p.classList.add('flex');
    p.setAttribute('aria-hidden', 'false');
    const input = cache('paletteInput');
    input.value = '';
    input.focus();
    renderPalette('');
  }
  function closePalette() {
    const p = cache('palette');
    if (p.classList.contains('hidden')) return; // already closed (e.g. stray Escape)
    p.classList.add('hidden');
    p.classList.remove('flex');
    p.setAttribute('aria-hidden', 'true');
    restoreFocus(paletteReturnFocus);
    paletteReturnFocus = null;
  }
  function renderPalette(q) {
    q = q.trim().toLowerCase();
    paletteItems = PALETTE_ITEMS.filter(
      (it) => !q || it.label.toLowerCase().includes(q) || it.cat.includes(q)
    );
    // add ad-hoc "search for q" entry if user typed something
    if (q) {
      paletteItems.unshift({
        cat: 'run',
        label: `search: ${q}`,
        hint: '/api/v1/search',
        action: () => {
          STATE.query = q;
          cache('searchInput').value = q;
          runSearch();
        },
      });
    }
    paletteIndex = 0;
    cache('paletteList').innerHTML = paletteItems
      .map(
        (it, i) => `
        <li data-idx="${i}" class="palette-item px-3 py-2.5 rounded-[7px] flex items-center gap-3 cursor-pointer mono text-[13px] ${i === 0 ? 'bg-[var(--color-ink-3)]' : ''}">
          <span class="chip" style="min-width: 36px; justify-content: center;">${escapeHtml(it.cat)}</span>
          <span class="text-[var(--color-fog-0)]">${escapeHtml(it.label)}</span>
          <span class="text-[var(--color-fog-4)]">${escapeHtml(it.hint)}</span>
          <span class="flex-1"></span>
          ${i === 0 ? '<span class="mono text-[10px] text-[var(--color-fog-4)]">↵ select</span>' : ''}
        </li>
      `
      )
      .join('');
    $$('#paletteList .palette-item').forEach((el, i) => {
      el.addEventListener('click', () => {
        paletteItems[i].action();
        closePalette();
      });
      el.addEventListener('mouseenter', () => {
        paletteIndex = i;
        highlightPalette();
      });
    });
  }
  function highlightPalette() {
    $$('#paletteList .palette-item').forEach((el, i) => {
      el.classList.toggle('bg-[var(--color-ink-3)]', i === paletteIndex);
    });
  }

  // ---------- wiring ----------
  function runExample(q) {
    STATE.query = q;
    cache('searchInput').value = q;
    runSearch();
    window.scrollTo({ top: cache('resultsSection').offsetTop - 80, behavior: 'smooth' });
  }

  function wire() {
    let t;
    cache('searchInput').addEventListener('input', (e) => {
      clearTimeout(t);
      STATE.query = e.target.value;
      t = setTimeout(() => runSearch(), 220);
    });
    cache('searchInput').addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        clearTimeout(t);
        runSearch();
      }
    });

    $$('.chip[data-filter]').forEach((el) => {
      el.addEventListener('click', () => {
        const k = el.dataset.filter;
        const stateKey = k === 'include-insecure' ? 'includeInsecure' : k === 'all-depths' ? 'allDepths' : k;
        if (stateKey === 'allDepths') {
          const parsed = parseQuery(STATE.query);
          if (!(STATE.filters.version || parsed.ver)) {
            showToast('all-depth search requires a version');
            return;
          }
        }
        cycleFilter(stateKey);
        if (stateKey === 'exact' && STATE.filters.exact) STATE.filters.allDepths = false;
        if (stateKey === 'allDepths' && STATE.filters.allDepths) STATE.filters.exact = false;
        renderFilterChips();
        runSearch();
      });
    });

    $$('.chip.example').forEach((el) => {
      el.addEventListener('click', () => runExample(el.textContent.trim()));
    });

    $$('[data-view]').forEach((el) => {
      el.addEventListener('click', () => {
        $$('[data-view]').forEach((o) => o.classList.toggle('seg-btn--active', o === el));
        STATE.view = el.dataset.view;
        syncUrl();
        // re-render cached results in new view mode (no API call)
        refreshCurrentView();
        // scroll into view only when results are below the fold
        const rs = cache('resultsSection');
        if (rs && rs.getBoundingClientRect().top > window.innerHeight - 80) {
          window.scrollTo({ top: rs.offsetTop - 80, behavior: 'smooth' });
        }
      });
    });

    $$('[data-close]').forEach((el) => el.addEventListener('click', closeDrawer));

    cache('paletteTrigger').addEventListener('click', openPalette);
    $$('[data-palette-close]').forEach((el) => el.addEventListener('click', closePalette));
    cache('paletteInput').addEventListener('input', (e) => renderPalette(e.target.value));
    cache('paletteInput').addEventListener('keydown', (e) => {
      const items = $$('#paletteList .palette-item');
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        paletteIndex = Math.min(items.length - 1, paletteIndex + 1);
        highlightPalette();
      } else if (e.key === 'ArrowUp') {
        e.preventDefault();
        paletteIndex = Math.max(0, paletteIndex - 1);
        highlightPalette();
      } else if (e.key === 'Enter') {
        e.preventDefault();
        if (items[paletteIndex]) items[paletteIndex].click();
      }
    });

    window.addEventListener('keydown', (e) => {
      const inField = ['INPUT', 'TEXTAREA'].includes(document.activeElement?.tagName);
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        openPalette();
      } else if (e.key === '/' && !inField) {
        e.preventDefault();
        cache('searchInput').focus();
      } else if (e.key === 'Escape') {
        closePalette();
        closeDrawer();
      }
    });
  }

  function applyViewToggleStyles() {
    $$('[data-view]').forEach((o) => {
      o.classList.toggle('seg-btn--active', o.dataset.view === STATE.view);
    });
  }

  function hasActiveState() {
    const f = STATE.filters;
    return !!(
      STATE.query ||
      f.version ||
      f.arch ||
      f.license ||
      (f.sort && f.sort !== 'relevance') ||
      f.exact ||
      f.allDepths ||
      f.includeInsecure ||
      STATE.page > 1
    );
  }

  // ---------- init ----------
  hydrateFromUrl();
  wire();
  cache('searchInput').value = STATE.query;
  renderFilterChips();
  applyViewToggleStyles();
  if (hasActiveState()) {
    runSearch({ resetPage: false });
  } else {
    renderWelcome();
  }
  loadBoot();
})();
