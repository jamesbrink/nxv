# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.7.3](https://github.com/utensils/nxv/compare/v0.7.2...v0.7.3) - 2026-08-12

### Fixed

- scope insecure flag by package name, not version string ([#81](https://github.com/utensils/nxv/pull/81))

## [0.7.2](https://github.com/utensils/nxv/compare/v0.7.1...v0.7.2) - 2026-08-10

### Fixed

- *(nix)* use `nxv sync` to refresh the index in the NixOS module ([#78](https://github.com/utensils/nxv/pull/78))

## [0.7.1](https://github.com/utensils/nxv/compare/v0.7.0...v0.7.1) - 2026-08-04

### Fixed

- *(cli)* accept conventional boolean environment values ([#76](https://github.com/utensils/nxv/pull/76))

## [0.7.0](https://github.com/utensils/nxv/compare/v0.6.1...v0.7.0) - 2026-07-30

### Added

- *(cli)* add run shell helper ([#74](https://github.com/utensils/nxv/pull/74))

## [0.6.1](https://github.com/utensils/nxv/compare/v0.6.0...v0.6.1) - 2026-07-23

### Fixed

- *(skill)* correct historical package examples ([#73](https://github.com/utensils/nxv/pull/73))

### Other

- *(repo)* remove the vestigial nixpkgs submodule ([#71](https://github.com/utensils/nxv/pull/71))

## [0.6.0](https://github.com/utensils/nxv/compare/v0.5.0...v0.6.0) - 2026-07-23

### Added

- *(cli)* [**breaking**] split application update from index sync ([#67](https://github.com/utensils/nxv/pull/67))
  `nxv update` now only updates the nxv application; the new `nxv sync` command
  downloads or refreshes the package index. Old index-related `nxv update` flags
  are rejected with migration guidance, and the retired `NXV_NO_SELF_UPDATE`
  variable is rejected when set. `nxv skill install` now requires an explicit
  target: agent names, `--detected`, or `--all`.

### Fixed

- *(cli)* clarify sync and skill guidance ([#67](https://github.com/utensils/nxv/pull/67))
- *(release)* keep blank line before new changelog sections ([#70](https://github.com/utensils/nxv/pull/70))

### Other

- *(release)* carry breaking-change details into the changelog ([#69](https://github.com/utensils/nxv/pull/69))

## [0.5.0](https://github.com/utensils/nxv/compare/v0.4.4...v0.5.0) - 2026-07-22

### Added

- Blueprint design-system UI refresh (web app + docs) ([#63](https://github.com/utensils/nxv/pull/63))
- [**breaking**] emit license/maintainers/platforms as real JSON arrays ([#56](https://github.com/utensils/nxv/pull/56))

### Fixed

- *(history)* resolve full history by attribute path ([#66](https://github.com/utensils/nxv/pull/66))
- *(search)* scope version queries to nearest attributes ([#62](https://github.com/utensils/nxv/pull/62))
- *(search)* preserve relevance ordering by default ([#61](https://github.com/utensils/nxv/pull/61))
- *(info)* match package names exactly instead of by prefix ([#57](https://github.com/utensils/nxv/pull/57))
- *(search)* honor --exact when a version filter is present ([#58](https://github.com/utensils/nxv/pull/58))

### Other

- sync all documentation with the code ([#65](https://github.com/utensils/nxv/pull/65))

## [0.4.4](https://github.com/utensils/nxv/compare/v0.4.3...v0.4.4) - 2026-07-21

### Fixed

- *(ci)* tolerate index manifest propagation delay

### Other

- sync prefix index documentation ([#51](https://github.com/utensils/nxv/pull/51))
- *(db)* accelerate cold prefix searches ([#50](https://github.com/utensils/nxv/pull/50))

## [0.4.3](https://github.com/utensils/nxv/compare/v0.4.2...v0.4.3) - 2026-06-15

### Other

- *(db)* optimize schema v4 local queries ([#46](https://github.com/utensils/nxv/pull/46))

## [0.4.2](https://github.com/utensils/nxv/compare/v0.4.1...v0.4.2) - 2026-06-13

### Fixed

- *(index)* recover scheduled publisher

## [0.4.1](https://github.com/utensils/nxv/compare/v0.4.0...v0.4.1) - 2026-06-13

### Fixed

- *(publish)* make index release uploads resilient

## [0.4.0](https://github.com/utensils/nxv/compare/v0.3.0...v0.4.0) - 2026-06-12

### Added

- *(skill)* add nxv skill subcommand (list/install/uninstall/show)

### Fixed

- *(flake)* let the embedded skill template through the crane source filter
- *(update)* retry self-update check without a rejected GITHUB_TOKEN

### Other

- *(skill)* address copilot review (list --ascii docs, grammar nit)
- *(skill)* ground the default project-pair claim in agents' read paths
- *(skill)* rustfmt integration tests
- *(skill)* document nxv skill across README, AGENTS.md, and website
- *(skill)* add integration tests and ship generated skill copies in-repo
- sweep all docs for post-v4/0.3.0 accuracy

## [0.3.0](https://github.com/utensils/nxv/compare/v0.2.1...v0.3.0) - 2026-06-11

### Added

- *(stats)* per-channel release coverage in nxv stats and /api/v1/stats
- *(publish)* default min_version to the schema version; fix update UX for incompatible indexes
- *(indexer)* wire the snapshot ingestion coordinator
- *(indexer)* add eval-based ingestion (eval.rs)
- *(indexer)* add data-quality gates and run report (monitor.rs)
- *(indexer)* add streaming snapshot parser (snapshot.rs)
- *(indexer)* add S3 channel-release discovery (releases.rs)
- *(db)* [**breaking**] schema v4 — observation model, releases ledger, retire git indexer core

### Fixed

- expand tilde in --db-path / NXV_DB_PATH once at startup
- *(indexer)* create the db path's parent dir, not the default data dir
- address CI clippy failures and Copilot review findings
- *(indexer)* parse stable/beta release names; declare rust-version 1.95
- *(monitor)* two-threshold baseline — warn on moderate drops, fail only catastrophic
- *(monitor)* date-bound the rolling baseline so backfills aren't held to newer data
- *(indexer)* drop stale pooled connections; retry mid-body download failures
- *(indexer)* harden the pipeline per adversarial review
- *(output)* flake-epoch padding, Nix attr quoting, and search scaling for v4
- *(indexer)* accept string envelope version; prune planning with --since
- *(output)* use full commit hashes in generated nix commands
- *(ci)* remove nightly clippy formatting borrows ([#35](https://github.com/utensils/nxv/pull/35))

### Other

- *(flake)* add nxv-indexer to flake checks
- rewrite /release skill and AGENTS.md for the release-plz flow
- adopt release-plz for release automation
- snapshot indexer documentation and gated publish workflow
- *(indexer)* end-to-end tests against a mock release bucket
- *(indexer)* validate the nix-env era against a real 2018 release
- *(deps)* update Rust and Nix dependencies
- make AGENTS.md the single source of truth
- *(direnv)* bootstrap nix-direnv 3.1.1 for cached devshell entry

## [0.2.1] - 2026-04-24

_Ships an agent-facing Claude Code skill so Claude / openclaw can drive
nxv directly, and fixes #33 — `--format json` is now pure JSON and
broken pipes (`| head`, `| jq`) exit cleanly instead of panicking._

### Added

- Claude Code skill at `.claude/skills/nxv/SKILL.md` so Claude (and any
  agent that consumes the open Agent Skills standard, e.g. openclaw)
  can drive the nxv CLI and HTTP API on the user's behalf without
  extra setup. Documented at `website/guide/skill.md`. (#34)

### Fixed

- `--format json` and `--format plain` no longer emit human-readable
  header lines (`Version history for: ...`, `Package: foo X.Y`, `To use
  this version: ...`, `Package 'X' not found.`) before the machine
  output. JSON output now parses cleanly with `jq` / `python -m json.tool`
  / `dasel` etc., matching the documented contract. Empty / not-found
  results emit `[]` instead of a prose line. Affects `nxv history` (all
  modes) and `nxv info`. (#33, #34)
- Broken pipes (e.g. `nxv search ... --format json | head -1`,
  `nxv history ... | jq`) no longer panic out of `println!` with a
  spurious "failed printing to stdout: Broken pipe" backtrace and exit
  code 5. nxv now resets `SIGPIPE` to its default disposition at startup
  on Unix, matching ripgrep / fd / bat behavior, so the process exits
  cleanly when a downstream consumer closes the pipe early. (#33, #34)

## [0.2.0] - 2026-04-23

_`nxv update` now also checks for a newer nxv binary release. Local
installs are replaced atomically; Nix / cargo / Homebrew installs get a
package-manager-native upgrade hint._

### Added

- `nxv update` now runs a binary self-update check after refreshing the
  package index. Detects the install method (Nix, `cargo install`,
  Homebrew, or local) and behaves appropriately (#32):
  - **Local install** — downloads the platform release binary, verifies
    its SHA-256 against `SHA256SUMS.txt`, and atomically replaces the
    running executable.
  - **Nix / cargo / Homebrew** — leaves the binary alone and prints the
    matching upgrade command (hints respect `--force`/`--version` where
    the underlying package manager supports it).
- `--no-self-update` flag (plus `NXV_NO_SELF_UPDATE` env var) on
  `nxv update` to opt out of the binary check — used internally by the
  first-run prompt for a missing index so search/info never surprise a
  user with a binary replacement.

### Changed

- Binary check is best-effort: a GitHub outage or rate limit is
  surfaced as a warning, not a failure — the index refresh has already
  succeeded at that point.

## [0.1.7] - 2026-04-22

_Web UI bundling + cards view: ships fully offline assets (Tailwind +
variable fonts, no CDN load) and a working cards layout for the
search-results view toggle._

### Added

- Web UI: cards view for search results (issue #30). Toggle `cards`/`rows`
  re-renders cached results instantly without re-querying the API. Cards
  use a responsive `auto-fill, minmax(296px)` grid with hover lift, a
  brand-coloured accent rail, dotted meta divider (first / last / rev),
  and three icon+label actions (flake / run / history).
- Web UI: bundle Tailwind + Inter/JetBrains Mono variable fonts for true
  offline operation (issue #28). Removes the runtime CDN load
  (`@tailwindcss/browser` + Google Fonts) and embeds the assets in the
  Rust binary via `include_str!`/`include_bytes!`. Adds `tw` and
  `tw-dev` devshell helpers (Tailwind v4 CLI), `frontend/package.json`,
  `frontend/tailwind.src.css` (now the canonical theme source), and
  routes `/tailwind.css`, `/fonts.css`, `/fonts/{name}` in the server
  with the same 24h cache layer as the rest of the static assets.
- Docs site theme rebuilt to mirror the embedded web UI at
  `frontend/index.html`: oklch ink/fog palette, Nix-blue accent,
  JetBrains Mono on UI chrome, 2px radii, hairline panel borders,
  scanline flourish on the hero. Dark mode ports the app's `@theme`
  tokens directly; a matching light mode is designed fresh on the same
  255 hue axis. Targets WCAG 2.1 AA.
- Standalone SVG variants of the app's bracket monogram
  (`website/public/nxv-logo-{light,dark}.svg`) wired into
  `themeConfig.logo` (with `alt: "nxv"` for accessibility) and the
  favicon.
- Four devshell helpers — `docs-dev`, `docs-build`, `docs-preview`,
  `docs-fmt` — for running the VitePress site locally; adds `pkgs.bun`
  to the devshell.
- Missing docs content: `dedupe` subcommand and `NXV_FRONTEND_DIR` env
  var in the guide, `GET /api/v1/metrics` in the API reference.

### Changed

- Bumped VitePress 1.6.0 → 1.6.4, Vue 3.5.0 → 3.5.33, tailwindcss and
  `@tailwindcss/vite` 4.1.0 → 4.2.4, prettier 3.4.0 → 3.8.3.
- `server.strictPort: false` in the Vite config so `docs-dev` falls
  through to the next free port instead of erroring.

### Fixed

- Web UI: long version strings (e.g. full git hashes) in row view now
  render as `xxxxxxxxxx…` with a `title=` tooltip carrying the full
  string instead of being silently sliced.
- Web UI: `truncateName` now strips trailing 7+ char hex suffixes
  (e.g. `vimpager-a4da4dfac…` → `vimpager`) instead of greedy-trimming
  to the first hyphen, which over-trimmed legitimate names like
  `prefetch-npm-deps-2.0.0`.
- Web UI: view-toggle no longer auto-scrolls to the results section on
  every click — only when the section is below the fold.
- Docs: stale `/api/v1/health` response example (`version: 0.6.0` →
  `0.1.6`).
- Docs: wrong indexer schema description (v4 → actual v3, integer
  commit dates, removed fabricated `version_source`/`store_path`/
  `is_insecure` columns and the non-existent "Store Path Extraction"
  section).
- Docs: misleading "store paths from cache.nixos.org" claim in the
  Getting Started guide.
- `website/.prettierignore` now excludes the gitignored vitepress
  server cache so `fmt:check` passes cleanly.
- Flaky `test_batch_insert_10k_performance` threshold: was truncating
  seconds (so 5.99s passed while 6.00s failed), now compares
  milliseconds against a generous 30s ceiling that still detects
  order-of-magnitude regressions but no longer trips on a busy Nix
  sandbox.

## [0.1.6] - 2026-04-22

### Added

- `nxv dedupe` subcommand (feature-gated under `indexer`) that collapses
  duplicate `(attribute_path, version)` rows created by the pre-0.1.5
  incremental-indexer bug. For each pair with more than one row it keeps
  the earliest `first_commit_*` and the latest `last_commit_*` across the
  duplicates and deletes the rest, then optionally `VACUUM`s. Supports
  `--dry-run` for projected stats without mutation and `--no-vacuum` for
  faster runs when disk reclaim isn't needed. Verified against the
  currently-published bloated index: 1,745,607 → 195,071 rows (-89%),
  1.8GB → 310MB (-82%), `search firefox` ~15s → ~100ms. (#25)

## [0.1.5] - 2026-04-21

### Fixed

- Incremental indexer no longer creates a duplicate row per
  `(attribute_path, version)` on every run. Each run seeded a fresh
  `open_ranges` map and restamped every still-present package with a new
  `first_commit_hash`; `INSERT OR IGNORE` didn't catch it because the
  UNIQUE constraint includes `first_commit_hash`. Over ~480 CI runs this
  grew the published index to 1.8GB / 1.7M rows against only ~120k
  distinct `(attribute_path, version)` pairs (some duplicated 290×),
  making queries that used to be sub-second take ~15s. Fix seeds
  `open_ranges` from the last checkpoint, switches batch insert to
  `INSERT ... ON CONFLICT DO UPDATE` (with sticky `source_path`), flushes
  all open ranges at every periodic checkpoint so a hard kill still
  resumes correctly, and adds `idx_packages_last_commit_hash` to keep the
  resume-seed query fast on large DBs. Existing bloated DBs still need a
  full rebuild to actually remove the duplicate rows. (#24)
- `load_open_ranges_at_commit` collapses duplicate
  `(attribute_path, version)` pairs deterministically via
  `MIN(id) GROUP BY` so seeding from an already-bloated DB picks a stable
  row.

### Changed

- `IndexResult::ranges_created` renamed to `ranges_written`; CLI now
  reports "Range rows written" since the count now includes upsert
  updates, not only inserts.
- Flake modernized: migrated from `flake-utils` to `flake-parts`, added
  `numtide/devshell` with categorized commands and an motd, added
  `treefmt-nix` (`nixfmt` + `rustfmt`). Bumped all flake inputs, refreshed
  the Rust toolchain to `stable.latest` with `rust-src` /
  `rust-analyzer` / `rustfmt` / `clippy`. Added `cargo-outdated`,
  `cargo-audit`, and `cargo-llvm-cov` to the devshell. Fixed Darwin
  `libiconv` linking (`LIBRARY_PATH` + `NIX_LDFLAGS` in `commonArgs`) and
  replaced `pkgs.nodePackages.prettier` with `pkgs.prettier` (nodePackages
  removed upstream). All existing outputs preserved: `overlays.default`,
  `nixosModules.default`/`nxv`, packages, apps, checks. (#24)
- Migrated repository references from `jamesbrink/nxv` to `utensils/nxv`.
- `publish-index` workflow: deep-fallback recovery, improved error
  handling, and `errexit` fix around GitHub API calls.
- Bumped `actions/checkout` to v6 in the FlakeHub workflow.
- CLAUDE.md: documented the local vs. remote backend abstraction,
  enumerated `NXV_*` environment variables, clarified that the indexer
  lives at `src/index/` while the Cargo feature is named `indexer`, and
  added `cargo bench`.

### Added

- VitePress documentation site with GitHub Pages deploy. (#22)
- Documentation badge in the README.

## [0.1.4] - 2026-01-08

### Added

- Schema forward compatibility with `min_version` field in manifest
- MIT LICENSE file

### Changed

- `/release` skill replaces `/release-notes` with full release automation

### Fixed

- `--force` flag now properly bypasses schema validation during update
- Incremental indexing now handles merge commits and `pkgs/by-name` paths

## [0.1.3] - 2026-01-04

### Added

- Structured JSON logging for API server with `--log-format json`
- Request tracing with unique request IDs
- Graceful shutdown handling for API server

### Changed

- Improved error handling throughout API server
- Better error messages for database and network failures

### Fixed

- Blocking database calls now use `spawn_blocking` to prevent runtime starvation
- Rate limiting and security hardening for API endpoints

## [0.1.2] - 2026-01-03

### Added

- Dynamic tab completion for package names in bash, zsh, and fish
- `/release-notes` command for pre-release checks
- `last_indexed_date` field in stats output

### Changed

- Improved stats output clarity and formatting
- README reorganized with better installation instructions

### Fixed

- DevShell now uses `bashInteractive` for proper readline and completion support

## [0.1.1] - 2026-01-03

### Added

- Cross-platform release builds for Linux x86_64, Linux aarch64, macOS x86_64,
  and macOS ARM64
- Static musl binaries for Linux

### Fixed

- crates.io version check for first-time publish
- Excluded large files from crate package

## [0.1.0] - 2024-12-30

### Added

- Initial release of nxv (Nix Versions)

#### Search

- Search packages by name with prefix matching
- Exact name matching with `--exact` flag
- Version filtering with `--version` flag
- Description search using FTS5 with `--desc` flag
- License filtering with `--license` flag
- Multiple output formats: table (default), JSON, plain text
- Sort options: date, version, name
- Result limiting with `--limit`
- Platform display with `--show-platforms`

#### Version History

- View all versions of a package with `nxv history <package>`
- Show specific version availability with `nxv history <package> <version>`

#### Index Management

- Download pre-built index with `nxv update`
- Force full re-download with `nxv update --force`
- Delta update support (infrastructure ready)

#### Index Statistics

- View index info with `nxv stats`
- Shows database size, commit range, and package counts

#### Shell Completions

- Generate completions with `nxv completions <shell>`
- Supports bash, zsh, fish, powershell, and elvish

#### Bloom Filter

- Fast O(1) negative lookups for exact name searches
- Instant "package not found" response for typos

#### Indexer (feature-gated)

- Build index from local nixpkgs clone with `nxv index`
- Incremental indexing from last indexed commit
- Full rebuild with `--full` flag
- Checkpoint and resume support with Ctrl+C handling
- Progress bars during indexing

### Technical Details

- SQLite database with FTS5 for full-text search
- Zstd compression for index distribution
- SHA256 verification for downloads
- Rust 2024 edition
- 10 MB release binary size

[0.2.1]: https://github.com/utensils/nxv/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/utensils/nxv/compare/v0.1.7...v0.2.0
[0.1.7]: https://github.com/utensils/nxv/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/utensils/nxv/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/utensils/nxv/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/utensils/nxv/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/utensils/nxv/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/utensils/nxv/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/utensils/nxv/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/utensils/nxv/releases/tag/v0.1.0
