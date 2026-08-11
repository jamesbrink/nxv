# nxv Indexer v2: Design Specification

> **Status: implemented & shipped** in nxv 0.3.0 (index schema v4, released
> 2026-06-11). This is a historical design artifact — for current behavior
> see `src/index/` and the root `AGENTS.md`.

> Driven by [ANALYSIS.md](./ANALYSIS.md). Revision 2 — incorporates the
> empirical verification sweep (S3 ground truth, pre-2021 eval testing,
> clone checks, codebase impact scan) and a three-judge design review.
> Hard requirements: (1) nxv stays a single binary — no external tool
> dependencies beyond `nix` itself (and `nix` only for eval-based paths);
> (2) index builds must surface missing-package anomalies early and loudly;
> (3) no user-visible capability regresses; (4) the index stays current
> (≤ hours behind nixpkgs master in steady state).

## 1. Core model

**Observation-based snapshot indexing.** Every row is backed by real
observations: "(attr, version) was present in channel release R at commit
C". No file→attr inference, no range stamping, no "no evaluation =
unchanged". The unit of work is a **channel release**, not a git commit.

| Era | Source | Mechanism | Nix eval? |
|---|---|---|---|
| 2020-03-27 → today | `packages.json.br` per release (S3), multi-channel | download → stream brotli → stream JSON parse | **No** |
| 2016-09-28 → 2020-03-27 | `nixexprs.tar.xz` per release (same S3 dirs) | `nix-env -f <tar> -qaP --json --meta` in subprocesses | Yes (~1,283 releases, one-time) |
| master HEAD (opt-in `--head-eval`) | GitHub tarball of master HEAD | chunked recursive extraction in memory-capped subprocesses | Yes (only when channels lag) |

Verified facts this rests on (see ANALYSIS appendix + verification sweep):
- `packages.json.br` exists for **2,866/2,866** releases from
  `nixpkgs-20.09pre218523.4a3f9aced7f` (2020-03-27) to today — zero gaps,
  14.6 GB compressed total (2.7→9.8 MB each; 71→381 MB decompressed).
- The nix-env era (1,283 releases back to 2016-09-28) evaluates **cleanly
  with current Nix** — one recipe, no old-Nix fallbacks (§6).
- Channel commits are ancestors of `origin/master` (226/226 verified) and
  Hydra-built — emitted `nix shell nixpkgs/<commit>#pkg` commands hit the
  binary cache (user-facing upgrade over arbitrary master commits).
- nix-env output is NOT top-level-only: it auto-recurses
  `recurseForDerivations` sets (~50–60% dotted attrs in every era).
- Aliases are never present in either source (`allowAliases = false`);
  darwin-only and unfree packages ARE present with correct `meta.platforms`.
- Channel-granularity capture: 26/27 (96%) of fast-mover master versions in
  a 6-month thunderbird/firefox/linux/nodejs basket; the only real miss was
  firefox 141.0.2 (~1.9-day lifetime). thunderbird 144.0 **never existed on
  master** (a single-commit `143.0.1 -> 144.0.1` double-jump) — no design
  can index it, and its absence is a *negative* regression fixture.

### Range semantics (explicit)

A row means: **(attr, version) was observed at `first_commit` and at
`last_commit`; presence at interior commits is NOT guaranteed.** A version
that disappears and later reappears (revert, security rollback, attr reuse)
collapses into one row spanning the interregnum — endpoints are real
observations, the interior is interpolation. This is accepted and must be
stated in user docs, API docs, and SKILL.md. The primary use case — "give me
a commit that has version X" — always gets a true endpoint.

## 2. Data sources & enumeration

- **S3 ListObjectsV2** on `nix-releases.s3.amazonaws.com` (public). Channels:
  - `nixpkgs/` → **nixpkgs-unstable**: historical spine (4,149 usable
    releases, 2016-09-28 → today, ~1.0–1.5 advances/day, stalls up to
    ~7 days recently / 17 historically).
  - `nixos/unstable-small/` → **nixos-unstable-small**: currency channel,
    typically **hours behind master** (measured 9h vs 1.4d for
    nixpkgs-unstable). Ingested from where its packages.json exists.
- **Source selection is a per-release probe**: every release tries
  `packages.json.br` first and, under `--backfill-evals`, falls back to
  nix-env on 404 — never decided by date or prefix (the boundary is
  mid-`20.09pre`, and 198 releases were renamed `21.03pre` before becoming
  `21.05pre`). Without that flag a 404 parks the release as `skipped` rather
  than starting an evaluation the run did not ask for. The plan-time date
  guess is only a worklist hint, and it must stay permissive enough that the
  probe actually runs: the filter excludes only releases dated before the
  first known artifact (2020-03-27), not every date-guessed nix-env row.
  The ledger source is corrected to the mechanism that actually produced the
  data.
- **Parsing requirements** (all verified the hard way):
  - Event-based XML parsing (quick-xml) per `<Contents>` block with
    optional elements — post-Feb-2025 objects embed
    `<ChecksumAlgorithm>`/`<ChecksumType>` between ETag and Size; a
    positional regex silently drops all 482 recent releases. Regression
    fixture with a post-2025 block is mandatory.
  - Release date = the `git-revision` object's LastModified (the flat HTML
    stub keys ceased 2025-02-20). S3 LastModified lags the commit's
    committer-date by hours (~15.5h measured) — see flake-epoch note in §8.
  - Skip-list: 28 ancient dirs (nixpkgs-0.x, `1.0preNNN_shortrev` with
    underscore, 14.04, no-`pre` formats), 7 re-uploaded `16.09pre` dirs
    (LastModified 2020-02-26 ≠ release date), and 18 `*-darwin/` channel
    CommonPrefixes.
  - Order releases by **S3 LastModified or numeric commit-count** — never
    lexicographic (`pre1001117` sorts before `pre880076`).
  - `git-revision` is always full 40-char; release-name shortrev drifts
    7→11→12 chars; `preNNNNNN == git rev-list --count` (free ingest-time
    sanity check).
- gsc.io cross-check: **dropped** — S3 is continuous from 2016-09 (max
  pre-2020 gap 23 days).
- Hydra's API is NOT used (slow/unreliable, verified).

## 2a. Currency guarantee ("always current")

- Steady state: each CI run (6h cadence) ingests new advances from both
  channels; staleness ≈ unstable-small lag (hours). Most 6h runs see 0–1 new
  releases and finish in minutes.
- Channel-stuck periods: `--head-eval` (on in CI) resolves master HEAD via
  the GitHub API, downloads the GitHub tarball (no git), runs the salvaged
  recursive extraction expression in memory-capped subprocess chunks, and
  upserts observations at the real master commit. Skipped while any channel
  observation is < 24h old, so it costs nothing in steady state.
- The monitor reports **index-head lag vs master** every run; `--strict`
  fails CI when lag > 72h. Staleness pages us; it never rots silently.

## 3. Database schema (v4)

```sql
CREATE TABLE package_versions (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,                -- top-level: pname; nested: final attr segment (see below)
    version TEXT NOT NULL,
    attribute_path TEXT NOT NULL,      -- full dotted path, segments unquoted
    first_commit_hash TEXT NOT NULL,
    first_commit_date INTEGER NOT NULL,
    last_commit_hash TEXT NOT NULL,
    last_commit_date INTEGER NOT NULL,
    description TEXT, license TEXT, homepage TEXT,
    maintainers TEXT, platforms TEXT, source_path TEXT,
    known_vulnerabilities TEXT,
    UNIQUE(attribute_path, version),
    CHECK(first_commit_date <= last_commit_date)   -- fresh DBs only; upsert enforces otherwise
);

CREATE TABLE releases (
    id INTEGER PRIMARY KEY,
    channel TEXT NOT NULL,
    release_name TEXT NOT NULL,
    commit_hash TEXT NOT NULL,          -- full 40-char from git-revision
    commit_count INTEGER,               -- preNNNNNN from the name
    release_date INTEGER NOT NULL,      -- git-revision object LastModified
    source TEXT NOT NULL,               -- packages_json | nix_env | head_eval
    status TEXT NOT NULL,               -- pending | ingested | failed | skipped
    attempts INTEGER NOT NULL DEFAULT 0,
    last_attempt_at INTEGER,
    attr_count INTEGER,
    error TEXT,
    ingested_at INTEGER,
    UNIQUE(channel, release_name)
);

CREATE INDEX idx_packages_search_nocase ON package_versions(
    attribute_path COLLATE NOCASE,
    version COLLATE NOCASE,
    (LENGTH(attribute_path) - LENGTH(REPLACE(attribute_path, '.', ''))),
    last_commit_date DESC,
    first_commit_date DESC
);
```

- **Writes**: order-agnostic widen-only upsert (CASE-WHEN bounds extension;
  `ON CONFLICT(attribute_path, version)`); metadata fields update only when
  the incoming observation's date ≥ the row's `last_commit_date` (newest
  observation wins; never COALESCE-flip names backward).
- **name normalization** (upstream pname conventions drifted ~2024:
  `python3.10-requests` → `requests`): top-level attrs store pname verbatim;
  nested attrs store the **final attrpath segment** (quotes stripped) —
  deterministic regardless of ingestion order. Note: production v3 rows
  store full name-with-version for ~27k rows; v4 is a behavior improvement
  to document.
- **FTS5**: AFTER UPDATE trigger gains
  `WHEN (old.name IS NOT new.name OR old.description IS NOT new.description)`
  so pure range-widening never touches FTS. For `--full`/catch-up ingest,
  triggers are dropped and FTS is rebuilt once at Finish
  (`INSERT INTO package_versions_fts(package_versions_fts) VALUES('rebuild')`).
  Measured: 28× writer slowdown without this.
- **Prefix-search covering index**: `idx_packages_search_nocase` supplies
  compact candidates for ASCII-case-insensitive prefix and prefix+version
  searches before full rows are fetched by primary key. Bulk ingestion drops
  it behind a durable marker and transactionally rebuilds it at finish; the
  next writable run repairs interrupted drops, and publication repairs or
  refuses an incomplete artifact.
- **Bloom filter**: full dotted attribute paths (quoted segments stored
  unquoted). Leaf-segment entries are NOT added (no consumer in the query
  path; leaf-name resolution is a possible fast-follow, see §12).
- **meta**: `schema_version=4`. The indexer **keeps writing**
  `last_indexed_commit` (= newest ingested release commit) and
  `last_indexed_date` — they are load-bearing for `manifest.latest_commit`,
  the `nxv sync` UpToDate check, `/health`, `/api/v1/stats`, and the
  frontend stats panel. Planning watermarks derive from `releases`.
- **Migration v3→v4 (local DBs)**: `dedupe_ranges()` (production DB has
  1,073 duplicate (attr,version) groups; without dedupe the new upsert fails
  at statement-prepare), then
  `CREATE UNIQUE INDEX uq_attr_version ON package_versions(attribute_path, version)`,
  then create `releases`, bump meta. No table rebuild; the stale 3-col
  UNIQUE stays as a redundant index; the CHECK applies to fresh DBs only.
- Deleted: `load_open_ranges_at_commit`, `update_package_range_end`,
  `idx_packages_last_commit_hash`, the dead delta machinery
  (`generate_delta_pack`, `db/import.rs` delta path — no delta has ever
  been published).

## 4. Pipeline architecture (src/index/)

```
mod.rs        coordinator: plan → ingest (parallel) → gate → write → finish
releases.rs   S3 listing/pagination (quick-xml), release parsing, skip-lists,
              git-revision fetch, plan diffing, retry/backoff state
snapshot.rs   one release → parsed snapshot: streaming .br download+decompress+
              JSON parse (serde_json MapAccess visitor; 381 MB raw never
              materialized), era-tolerant field handling (§5)
eval.rs       nix-env ingestion (pre-2020 era) + --head-eval master tarball:
              recycled memory-capped subprocesses, batch bisection on failure
monitor.rs    gates BEFORE write: count ladder, sentinels, births/deaths,
              stall advisory, head-lag; end-of-run report (stdout + JSON)
```

Flow per run (`nxv index`):
1. **Plan**: list S3 per channel → upsert unknown releases `pending` →
   work list = `pending` + retryable `failed` (skip unless
   `now − last_attempt_at > base × 2^attempts`; after 5 attempts →
   terminal `skipped`, resurrectable via `--retry-failed`), ordered oldest→
   newest by release_date.
2. **Ingest** (N parallel workers, default `min(4, cores)`): fetch + parse
   one release into `HashMap<attr, Entry>`. Any mid-stream brotli/JSON error
   discards the whole map — partial snapshots never reach the writer.
3. **Gate** (in-order by release, seq-numbered buffer): monitors run on the
   parsed map BEFORE any write (§7). Gate-failed releases are marked
   `failed` and their data is dropped — a bad snapshot can never pollute
   `package_versions`.
4. **Write**: gate-passed snapshots feed a **multi-release range
   aggregator** keyed (attr, version) that merges K consecutive releases
   per flush group (K≈64 for full/catch-up, K=1 incremental) — row writes
   become O(distinct pairs + metadata changes), not O(alive attrs ×
   releases) (measured: ~63 new pairs per advance vs 144k alive rows).
   Single writer thread, bounded channel, ≤50k rows per transaction;
   releases in a group are marked `ingested` in the same transaction as
   their rows.
5. **Finish**: FTS rebuild (if triggers were dropped), bloom rebuild, write
   meta watermarks, print/write the coverage report.

Interrupt safety: ctrlc flag checked between releases; a killed run leaves
`pending`/`failed` rows that the next run picks up. No checkpoint hash
exists to strand.

HTTP policy: one shared reqwest client; explicit connect/read timeouts;
3–5 retries with exponential backoff + jitter on 5xx/timeouts;
`User-Agent: nxv-indexer/<version>`; ≤4 concurrent S3 downloads.

## 5. Snapshot parser tolerance (verified per-era)

- Envelope `{"version": 2, "packages": { <attrpath>: {name, pname, version,
  system, meta} }}` in ALL eras — assert `version == 2`.
- Attr keys: ~83% dotted in every era, ≤6 dots deep; non-identifier segments
  are Nix-quoted inside the key (`aspellDicts."or"`, `emacsPackages."@"`) —
  unquote segments for storage.
- `license`: dict | list[dict] | str | list[str] | empty list. `homepage`:
  str, occasionally a list (take first). `description`/`license`/`platforms`/
  `position` absent on a large minority of entries.
- `meta.position` → source_path; strip the `/build/source/` prefix
  (2020-era files only) and the `:line` suffix.
- Ignore unknown meta fields (2023 added outputName/outputs; 2026 added
  identifiers/teams/maintainersPosition).
- `knownVulnerabilities`: list[str], present in all eras.
- nix-env era output shape `{attr: {name, pname, version, system, meta}}`
  — same handling.

## 6. Eval recipes (single-binary compatible)

- **Pre-2020 era**: extract `nixexprs.tar.xz`, run
  `nix-env -f <dir> -qaP --json --meta` with
  `NIXPKGS_ALLOW_{UNFREE,BROKEN,INSECURE,UNSUPPORTED_SYSTEM}=1` and
  **mandatory `--argstr system x86_64-linux`** (native eval of pre-2020
  nixpkgs fails on aarch64-darwin hosts). Verified: current Nix evaluates
  every era 2016–2020 with this one recipe; ~15–25 s and 0.5–1.4 GB RSS per
  release → default 4 workers, full backfill **~1.5–3 h** (not days).
  Optional `-A haskellPackages` second pass (~65 s/release) for fuller
  pre-2020 nested coverage — off by default.
- **--head-eval**: GitHub tarball + the salvaged recursive extraction
  expression (embedded; proven 119k-attr coverage) in chunked subprocesses;
  subprocess exit is the memory reclamation mechanism — no watchdogs, no
  rlimits, no in-process evaluator.

## 7. Monitoring (hard requirement: catch missing packages EARLY)

All gates run BEFORE the snapshot's rows are written (§4 step 3).

- **Count floor** = max of:
  - *year ladder* (absolute backstop, from measured totals 61k/69k/87k/101k/
    145k for 2020/2021/2022/2023/2026; nix-env era ladder from measured
    counts, floor 10k at 2016): a table in code, interpolated by year;
  - *rolling baseline*: 0.90 × max(attr_count) over the last 10 `ingested`
    releases of the **same channel AND same source era** (prevents
    ratcheting-down; baseline can't decay just because regressions passed).
  - First-ever release of a channel/era: absolute floor only.
  - The one-time +≈33k birth event at the 2020-03-27 era boundary is
    whitelisted.
- **Sentinels** (pattern-based, with applicability windows — verified
  against real data): `firefox` (always), `thunderbird` (always),
  regex `python3\d+Packages\.requests` (entire history — the unversioned
  `python3Packages.*` set exists in NO era), `nh` (≥ 2024-01). Extensible
  via flag. Missing sentinel ⇒ gate failure.
- **Births/deaths accounting** per release: log `+births −deaths Δnet
  (total)`; deaths > 5% in one advance ⇒ warning with samples (mass renames
  are legitimate; advisory unless `--strict`).
- **Cumulative drift**: HEAD attr count vs rolling max and vs the
  search.nixos.org package-count floor — catches slow leaks that per-release
  ratchets miss.
- **Stall handling**: advance gaps are NOT anomalies (6.6-day stalls occur
  while healthy); "no new release in >10 days" is an advisory. Head-lag
  >72h under `--strict` fails CI (§2a).
- **Report**: stdout summary + `--report report.json` (releases
  ingested/failed/skipped, counts, sentinel table, births/deaths events,
  head lag); CI publishes it as a job summary.
- **Regression fixtures** (tests, served from a local mock S3): thunderbird
  142.0 must NOT span 2025-08→2026-01; thunderbird 143.0 present;
  thunderbird **144.0 ABSENT** (never existed on master); `nh` present;
  a `python313Packages.*` attr present and bloom-resolvable.

## 8. Command generation under observation dates

- `predates_flakes()` (src/db/queries.rs) and its duplicate in
  frontend/app.js compare against the flake-epoch commit date. Observation
  dates lag commit dates (hours–17 days), so the boundary must be replaced
  by the **first channel release whose tree contains flake.nix**
  (determined once during implementation, hardcoded alongside FLAKE_EPOCH)
  — otherwise early-2020 rows emit flake refs against pre-flake commits.
- Attr segments that required quoting in Nix (44 keys currently) must be
  re-quoted in emitted commands (`nixpkgs/<hash>#aspellDicts."or"`), in both
  CLI and frontend.
- Legacy (pre-flake) `fetchTarball` commands may need
  `{ system = "x86_64-linux"; }` pinning on darwin hosts — document in the
  website guide; optionally emit the pinned form when `platforms` shows no
  darwin support.

## 9. CLI surface & feature parity

**No user-visible capability regresses.** Parity matrix:

| Current feature | v2 fate |
|---|---|
| `search` / `info` / `history` / `stats` / `serve` / API / web UI / completions | Unchanged surface; `stats` adds optional release-coverage block (serde(default) fields, v3-DB tolerant); search gains depth-first ranking + SQL-level LIMIT (§12) |
| All metadata fields | Kept — verified present in BOTH source eras (incl. knownVulnerabilities and position) |
| Insecure-package command prefixes | Unchanged |
| FTS5 name/description search | Unchanged behavior (name column becomes pname/leaf — strictly better; documented) |
| Bloom fast-negative lookup | Kept, learns dotted paths automatically |
| `update` (full download, minisign verify, self-update) | Unchanged; delta code path was dead (never published) and is removed |
| `publish` | Unchanged format; **min_version now defaults to SCHEMA_VERSION in code** (§10) |
| `dedupe` | Kept — it is the v3→v4 migration primitive and still repairs old DBs |
| `backfill`, `reset` | Retired as visible commands (their *purpose* — repairing missing metadata, resetting a checkout — no longer exists: metadata is complete by construction and no checkout is used). Hidden deprecation stubs explain this for one release cycle |
| `index --full/--since/--until` | Kept (same semantics over releases) |
| Index freshness | Met or beaten (§2a) |

`nxv index` flags: `--channel` (repeatable; default both), `--since`,
`--until`, `--jobs`, `--strict`, `--report <path>`, `--retry-failed`,
`--head-eval`, `--backfill-evals`, `--full`. `--nixpkgs-path` accepted as a
hidden no-op with a deprecation warning for one release cycle.

## 10. Migration & rollout sequencing (ordered, mandatory)

1. **Code**: SCHEMA_VERSION=4, MIN_READABLE_SCHEMA=4 (v3 DBs remain
   readable: their effective min is 3 ≤ 4). `nxv publish` **defaults
   `min_version` to SCHEMA_VERSION and refuses to publish a schema-4 DB
   with min_version unset or < 4** (today the doc claims this default but
   the code passes None — publishing v4 ungated would let every old
   client's `nxv sync` overwrite its working v3 index and then fail to
   open it: verified landmine L1).
2. **Sync compatibility guidance**: on IncompatibleIndex, `nxv sync`
   directs users to run `nxv update` (or the printed package-manager command)
   and then retry `nxv sync`; it never checks the release API itself.
3. **Release the new binary first** (reads v3 and v4; index format
   unchanged at this point). Wait an adoption window (1–2 weeks).
4. **Initial v4 full rebuild runs out-of-band** (locally — hours for the
   packages.json era + ~2–3h backfill), validated with the §7 report +
   fixtures, then published manually with min_version=4.
5. publish-index.yml flips to the new flow (§11) and passes
   `--min-version 4` explicitly (belt-and-braces).
6. Update issues #5/#21/#23, AGENTS.md, SKILL.md, website guide.
7. Keep the old DB for diffing; `manifest.version` stays at its current
   value (it gates manifest *structure*, not DB schema).

## 11. CI (publish-index.yml)

Steps: download existing index → `nxv index --head-eval --report` →
**publish when ≥1 release was ingested** (or `force_publish`) → **alert
after publishing** (the job goes red when `healthy=false` or any release
failed).

> **Deviation from the original draft** (which gated publishing on "no
> pending/failed before the watermark"): publishing ingested progress is
> *always safe* — gate-failed snapshots never write rows, so the artifact
> only ever gains verified data, and the shipped ledger carries the failed
> release's retry/backoff state to the next run. Blocking the publish of 92
> good releases because 1 failed would only increase staleness without
> fixing the hole; retries fix the hole. The hole is surfaced instead:
> `unsettled_before_watermark` in the report/stats, plus the red workflow
> run. Head-lag breaches mark the run unhealthy unconditionally.

Most 6h runs ingest 0–2 releases and exit in ~2–3 min without republishing
the artifact. No nixpkgs clone, no clone-depth logic, no GitHub compare
API. `nxv stats` grep-scraping is replaced by the `--report` JSON.

## 12. Performance, size & search budgets

| Operation | Budget |
|---|---|
| Full rebuild, packages.json era (~2,866 releases, ~14.6 GB) | hours, parse-bound; 4 workers × ~250 MB + writer + aggregator ≈ **<2 GB RSS** |
| Backfill 2016–2020 (~1,283 nix-env runs, 15–25 s each) | **~1.5–3 h**, 4 workers × ~1.5 GB |
| Incremental (CI) | seconds–minutes, <500 MB; ~70% of runs ingest nothing and skip publish |
| Row count, full history | ~1.77 M (attr,version) pairs as of 2026-07 (+~170k/yr) |
| DB size | ~2.1 GB raw / ~220 MB zstd as of 2026-07, including the covering prefix index; **artifact target ≤250 MB zstd**. If exceeded, intern license/platforms/maintainers into dictionary side-tables behind the existing query API (measured ~50× metadata duplication — the named lever, not a v1 requirement) |
| FTS | dropped-trigger ingest + one rebuild (~1–2 min at 1.2M rows) |
| Write amplification | aggregator keeps writes O(distinct pairs), not O(attrs × releases) |

Search under ~139k dotted attrs (verified hazards): prefix search on `python`
matches thousands of `python3xxPackages.*` rows. Prefix and prefix+version
queries use the covering ASCII-NOCASE index to rank at most 5,000 compact
candidate IDs, then fetch full rows by primary key with deterministic `id ASC`
tie-breaking. Ranking still orders shallower attr paths first
(`attribute_path` dot count ASC, then name), and SQL-level LIMIT pushdown keeps
pagination bounded.
Bare-leaf resolution (`requests` → `python313Packages.requests`) is a named
fast-follow, not v1.

## 13. Explicitly rejected alternatives

- **Per-commit evaluation (any variant)** — killed three branch lineages
  (ANALYSIS §2); produced months-long holes in practice.
- **nix-eval-jobs binary integration** — violates the single-binary
  requirement.
- **In-process Nix FFI** — Boehm-GC growth, build complexity, proven OOMs.
- **File→attr change mapping** — structurally incomplete; root cause of #23.
- **Date/prefix-based era selection** — refuted by the mid-prefix boundary
  and the 21.03pre rename; per-release probe only.
