---
name: nxv
description: Find any version of any Nix package across nixpkgs git history using the nxv CLI or HTTP API. Use when asked which nixpkgs commit shipped a specific package version (e.g. "python 2.7", "nodejs 15", "ruby 2.6"), when looking up package metadata/license/homepage, when generating a `nix shell nixpkgs/<commit>#pkg` invocation for an old version, or when querying the public/private nxv server. Triggers include "find python 2.7 in nixpkgs", "which commit had nodejs 15.14", "when was foo added/removed", "give me the nix shell command for ruby 2.6", "search nixpkgs for X", or any question about historical Nix package versions.
license: MIT
allowed-tools: Bash, Read, Glob, Grep
---

# nxv — Nix Version Index

`nxv` is a Rust CLI + HTTP API that indexes nixpkgs channel-release history (2016+) into a local SQLite database with a bloom filter for fast lookups. It answers: *"which exact nixpkgs commit shipped version X of package Y?"* and produces the `nix shell nixpkgs/<commit>#pkg` command you need to actually use it.

## Quick Reference

```bash
nxv search python                        # All python packages (most recent per version)
nxv search python 2.7                    # Filter by version (prefix match)
nxv search python --exact                # Exact attribute name only
nxv search "json parser" --desc          # Full-text search package descriptions
nxv run python 2.7                       # Resolve and open a pinned shell
nxv run python 3.11 --with nodejs@20     # Multi-package shell
nxv info python311                       # Detailed info for current version
nxv info python311 3.11.4                # Detailed info for specific version
nxv history python311                    # Version timeline (first/last seen)
nxv history python311 3.11.4             # When was 3.11.4 available?
nxv stats                                # Index statistics
nxv update                               # Update nxv itself to the latest release
nxv sync                                 # Download or refresh the local package index
nxv sync --force                         # Force a full index re-download
nxv serve --host 0.0.0.0 --port 8080     # Start HTTP API + web UI
nxv completions zsh                      # Generate shell completions
nxv skill install codex                  # Install this skill for one explicit agent
nxv skill install --detected             # Install for detected AI agents
nxv skill list                           # Agents, skill paths, install status
```

## How to Use This Skill

Parse `$ARGUMENTS` to determine the action:

- If arguments look like a **subcommand** (`search`, `run`, `info`, `history`, `stats`, `update`, `sync`, `serve`, `completions`, `skill`, and indexer-only `index`, `dedupe`, `publish`, `keygen`), run that subcommand.
- If arguments look like a **package name** (e.g. `python`, `nodejs 15`, `ruby 2.6`), default to `nxv search`.
- If arguments look like a **question** ("when was X added", "which commit has Y"), pick `search` or `history` accordingly.
- If no arguments, run `nxv stats` to give the user a quick health check of their index.

**For agents**: always pass `--format json` (CLI) or hit the HTTP API and pipe to `jq`. The table format is human-only; column widths are terminal-dependent. Exception: `nxv stats` has no `--format` flag — use `GET /api/v1/stats` when you need stats as JSON.

## Global Options

Every CLI invocation accepts:

| Flag                   | Description                                                 |
| ---------------------- | ----------------------------------------------------------- |
| `--db-path <PATH>`     | Path to the index database (default: platform data dir)     |
| `-v, --verbose`        | `-v` info, `-vv` debug (SQL queries, HTTP requests)         |
| `-q, --quiet`          | Suppress all output except errors                           |
| `--no-color`           | Disable colored output (also honors any non-empty `NO_COLOR`) |
| `--api-timeout <SECS>` | API request timeout when using remote backend (default: 30) |

## Local vs Remote Backend

`nxv` transparently runs against either a local SQLite index or a remote `nxv serve` instance. Set `NXV_API_URL` to switch:

```bash
# Use the public hosted instance — no local index needed
NXV_API_URL=https://nxv.urandom.io nxv search nodejs 15
NXV_API_URL=https://nxv.urandom.io nxv info python311

# Or your own private instance
export NXV_API_URL=http://gpu-host:8080
nxv search rust 1.70
```

If `NXV_API_URL` is unset, the CLI uses the local index at `~/Library/Application Support/nxv/index.db` (macOS) or `~/.local/share/nxv/index.db` (Linux). Run `nxv sync` to download the latest published index on first use.

## Search

Find packages and the commits where each version existed:

```bash
nxv search python                                    # Recent per (pkg, version)
nxv search python 3.11                               # Version prefix filter
nxv search python 2.7.3 --all-depths                 # Include nested package-set members
nxv search python --exact                            # Exact attribute name only
nxv search python --license MIT                      # Filter by license
nxv search python --sort date --reverse              # Oldest first
nxv search "web server" --desc                       # FTS5 description search
nxv search python --show-platforms                   # Add platforms column
nxv search python --full                             # All commits (no dedup)
nxv search python --limit 5                          # Cap at 5 results
nxv search python --format json                      # Machine-readable JSON
nxv search python --format plain                     # TSV for shell scripts
```

For version-qualified prefix searches, nxv first resolves the shallowest
attribute-path tier matching the package prefix, then applies the version prefix.
This keeps `python 3.11` on interpreter attributes such as `python311`; a query like
`python 2.7.3` reports a precise miss with nearby interpreter versions instead of
returning libraries whose own version is 2.7.3. A package-set query such as
`python27Packages 2.7.3` resolves its member depth naturally. Pass `--all-depths`
to request the legacy broad prefix behavior explicitly. The flag requires a version
and conflicts with `--exact` and `--desc`.

Version-miss diagnostics and suggestions are written to stderr. Successful JSON
searches return an array of package rows; an empty miss emits no stdout.

**JSON shape per row** (the same shape is returned by `nxv info`, `nxv history <pkg> <version>`, `nxv history --full`, and the `/api/v1` package endpoints):

```json
{
  "id": 9630,
  "name": "python3",
  "version": "3.12.13",
  "first_commit_hash": "d78e468770f4ab5e00c5015f4d77c1a499a76dc8",
  "first_commit_date": "2026-03-06T20:06:54Z",
  "last_commit_hash": "3d2613bc58a1f5b7805467a63a825e1d7bc9b7a9",
  "last_commit_date": "2026-07-21T12:39:35Z",
  "attribute_path": "python312",
  "description": "High-level dynamically-typed programming language",
  "license": ["Python-2.0"],
  "homepage": "https://www.python.org",
  "maintainers": ["mweinelt"],
  "platforms": ["x86_64-linux", "aarch64-darwin"],
  "source_path": "pkgs/development/interpreters/python/cpython/default.nix",
  "known_vulnerabilities": null
}
```

| Field                                                     | Type            | Notes                                                                          |
| --------------------------------------------------------- | --------------- | ------------------------------------------------------------------------------ |
| `id`                                                      | integer         | Index row id. Not stable across index rebuilds — never persist it.             |
| `name`                                                    | string          | Upstream derivation name. **Not installable** — see below.                     |
| `attribute_path`                                          | string          | The nixpkgs attribute. **This is what you install with.**                      |
| `version`                                                 | string          | Package version.                                                               |
| `first_commit_hash` / `last_commit_hash`                  | string          | Full 40-char nixpkgs commit hashes.                                            |
| `first_commit_date` / `last_commit_date`                  | string          | RFC 3339 / ISO 8601 UTC.                                                       |
| `description`, `homepage`, `source_path`                  | string \| null  | `source_path` is null for older packages.                                      |
| `license`, `maintainers`, `platforms`                     | array \| null   | Arrays of strings. `null` when the package declares none.                      |
| `known_vulnerabilities`                                   | array \| null   | `null` (or `[]`) means no known advisory; non-empty means the package is insecure. |

**`name` vs `attribute_path`** — these routinely differ and confusing them produces commands that fail. `name` is the upstream derivation name (`pname` for top-level attrs, the final attribute segment for nested ones); `attribute_path` is the address you actually install with. For the row above, `nix shell nixpkgs/<hash>#python312` works and `#python3` may not. **Always use `attribute_path`.**

`license`, `maintainers`, `platforms`, and `known_vulnerabilities` are real JSON arrays, so `jq` reaches them directly:

```bash
nxv search python312 --exact --format json | jq -r '.[0].license[]'        # Python-2.0
nxv search python312 --exact --format json | jq -r '.[0].platforms | join(", ")'
nxv search hello --format json | jq -r '.[] | select(.known_vulnerabilities != null) | .attribute_path'
```

> **Version note**: older nxv releases emitted these four fields as JSON-encoded *strings* (`"license": "[\"Python-2.0\"]"`), requiring a second `fromjson`. If you must support both, use `(.license | if type == "string" then fromjson else . end)`.

## Info

Detailed metadata for one package version (description, license, homepage, platforms, source path, known vulnerabilities):

```bash
nxv info python311                       # Latest known version
nxv info python311 3.11.4                # Specific version (positional)
nxv info python311 -V 3.11.4             # Specific version (flag form)
nxv info python311 --format json
```

`info` resolves the package name as an **exact attribute path** first, so it needs no
`--exact` flag: `nxv info python311 3.11.4` returns `python311` only, never
`python311Full` or `python311Packages.*`. If the package is known but never had the
requested version, `info` reports not found instead of falling back to unrelated prefix
matches — an empty result means "this package never had that version", not "try harder".

An unknown attribute path is widened to a prefix search, but what gets prefix-matched
depends on whether a version was given:

```bash
nxv info python311Packages.tk 3.11.4     # widened over ATTRIBUTE PATHS -> resolves
nxv info python311Packages.tk            # widened over the NAME field -> usually no match
```

So partial attribute paths generally only resolve when you also pass a version. For
open-ended prefix lookups use `nxv search` (with `--exact` as needed) instead.

## History

Version timeline — when each version first appeared and when it was last seen:

```bash
nxv history python311                    # All versions of python311
nxv history python311 3.11.4             # Just one version's window
nxv history ripgrep --full               # Add commits, license, homepage, etc.
nxv history python311 --format json
```

**JSON shape per row** — plain `nxv history <pkg>` returns this compact timeline shape:

```json
{
  "version": "3.11.4",
  "first_seen": "2023-06-15T00:00:00+00:00",
  "last_seen": "2023-12-01T00:00:00+00:00",
  "is_insecure": false
}
```

Note the field names differ from search (`first_seen`/`last_seen`, not `first_commit_date`/`last_commit_date`), and there are no commit hashes. Adding `--full`, or naming a version (`nxv history python311 3.11.4`), switches the output to the full search row shape documented above — use one of those when you need a commit hash to feed to `nix shell`. Like the compact timeline, bare `--full` resolves the package by its exact installable `attribute_path`.

## Using a Found Version

The quickest interactive path is `nxv run`, which resolves the same package and
version query as `search` and opens one pinned shell:

```bash
nxv run python 2.7
nxv run python 3.11 --with nodejs@20 --with jq
```

Additional `--with` values use `PACKAGE@VERSION` when a version is needed.
nxv resolves every query before launch, prefers an exact attribute match before
falling back to search's deterministic relevance rules, and uses each result's
latest observed commit. Modern revisions are combined in one `nix shell`; if
any result predates flakes, nxv uses one compatible `nix-shell -p` environment
instead. On Apple Silicon, the pre-flake fallback evaluates packages as
`x86_64-darwin` and requires Rosetta.

For manual command construction, take a `first_commit_hash` (or
`last_commit_hash`) from search/history output and feed it to Nix:

```bash
# Drop into a shell with that exact version
nix shell nixpkgs/e4a45f9#python

# Run it once
nix run nixpkgs/e4a45f9#python

# Add to a flake input
inputs.nixpkgs-python27.url = "github:NixOS/nixpkgs/<commit>";
```

Pick `first_commit_hash` for the canonical "introduced in" commit; pick `last_commit_hash` if you want the most recent commit that still shipped that version.

### Caveats for Old Commits

The direct `nix shell nixpkgs/<commit>#<attribute_path>` form may fail for old
nixpkgs revisions even when nxv found a valid observation:

- Modern Nix rejects the retired `edition` field in flake files from roughly
  2020 and earlier.
- Revisions from before late 2021 predate usable `aarch64-darwin` support for
  many packages. On Apple Silicon, evaluate the package as `x86_64-darwin` and
  run it through Rosetta when necessary.

Use a classic nixpkgs import with the full commit hash and the result's exact
`attribute_path`:

```bash
nix shell --impure --expr '
  (import (builtins.fetchTarball
    "https://github.com/NixOS/nixpkgs/archive/<full-hash>.tar.gz")
    { system = "x86_64-darwin"; }).ruby
' --command ruby --version
```

Replace `.ruby` and the command with the returned attribute and executable.
The `platforms` field comes from package metadata (`meta.platforms`); it is not
proof that the historical revision evaluates on that system or that Hydra
produced a cached binary for it.

## Application and Index Management

```bash
nxv update                               # Update the nxv application only
nxv sync                                 # Download or refresh the package index only
nxv sync --force                         # Force full re-download of the index
nxv sync --skip-verify                   # Skip minisign signature check (INSECURE)
nxv sync --public-key /path/key.pub      # Use a custom public key (self-hosted index)
nxv sync --manifest-url <URL>            # Use a custom manifest (self-hosted index)
nxv stats                                # Index size, commit range, last update
```

`nxv update` only checks GitHub for the latest nxv release:

- **Local install** (install.sh / manual download): downloads the platform binary, verifies SHA-256, atomically swaps the running executable.
- **Nix / cargo / Homebrew**: prints the matching upgrade hint (e.g. `brew upgrade nxv`) and exits successfully.

`nxv sync` independently refreshes the SQLite index and bloom filter. It never
checks for or replaces the application. If a published index requires a newer
schema, run `nxv update` (or the printed package-manager command) and retry
`nxv sync`.

## API Server

```bash
nxv serve                                            # 127.0.0.1:8080 (default)
nxv serve --host 0.0.0.0 --port 3000                 # Public bind
nxv serve --cors                                     # Enable CORS for all origins
nxv serve --cors-origins https://app.example.com     # Restrict CORS
nxv serve --rate-limit 10 --rate-limit-burst 20      # Per-IP rate limit
```

The server bundles:

- **Web UI** at `/` (Tailwind v4 + vanilla JS, embedded at build time)
- **OpenAPI docs** at `/docs` (Scalar UI)
- **REST API** at `/api/v1/*` — see endpoints below
- **Cache headers**: 1h on cacheable package routes; never on `/health`, `/metrics`

### HTTP Endpoints

All paths are under `/api/v1`. Wrapped responses always look like `{ "data": ..., "meta": {...} }` for paginated lists, `{ "data": ... }` for single items.

| Method | Path                                                | Purpose                              |
| ------ | --------------------------------------------------- | ------------------------------------ |
| `GET`  | `/search?q=<name>&limit=&offset=&sort=&exact=&all_depths=` | Search packages                 |
| `GET`  | `/search/description?q=<text>&limit=&offset=`       | Full-text search descriptions (FTS5) |
| `GET`  | `/packages/{attr}`                                  | All version records for a package    |
| `GET`  | `/packages/{attr}/history`                          | Version timeline (first/last seen)   |
| `GET`  | `/packages/{attr}/versions/{version}`               | All records for one version          |
| `GET`  | `/packages/{attr}/versions/{version}/first`         | First-seen commit                    |
| `GET`  | `/packages/{attr}/versions/{version}/last`          | Last-seen commit                     |
| `GET`  | `/stats`                                            | Index statistics                     |
| `GET`  | `/health`                                           | Liveness probe (uncached)            |
| `GET`  | `/metrics`                                          | Server metrics (uncached)            |

**Search query parameters:**

| Parameter | Type    | Description                          |
| --------- | ------- | ------------------------------------ |
| `q`       | string  | Search query (required)              |
| `version` | string  | Version filter (prefix match)        |
| `exact`   | boolean | Exact attribute name match           |
| `all_depths` | boolean | Include every attribute depth; requires `version` |
| `license` | string  | License filter                       |
| `sort`    | string  | `relevance` (default), `date`, `version`, or `name` |
| `reverse` | boolean | Reverse sort order                   |
| `limit`   | integer | Max results (default 50)             |
| `offset`  | integer | Results to skip (default 0)          |

**Quick examples against the public instance:**

```bash
curl -s "https://nxv.urandom.io/api/v1/search?q=python&version=3.11&limit=5" | jq
curl -s "https://nxv.urandom.io/api/v1/packages/python311/history" | jq '.data[0:3]'
curl -s "https://nxv.urandom.io/api/v1/packages/nodejs-15_x/versions/15.14.0/first" | jq
curl -s "https://nxv.urandom.io/api/v1/stats" | jq '.data'
```

Version-qualified search responses add an optional `meta.resolution` object with
`scope`, `resolved_depth`, `requested_version`, `version_matched`, optional
`deeper_matches_available`, and up to five `{attribute_path, version}` suggestions.
This metadata is additive; package rows and the CLI JSON array are unchanged.

## Skill Management

nxv can install this very skill for any major AI coding agent — the binary embeds the SKILL.md and writes it where each agent looks, per the [Agent Skills standard](https://agentskills.io):

```bash
nxv skill install codex                  # Install user-wide for one agent
nxv skill install --detected             # Explicitly install for detected agents
nxv skill install codex --project        # Install for Codex in the current project
nxv skill install --detected --project   # Map detected agents to project paths
nxv skill install claude codex           # Install for specific agents only
nxv skill install --all                  # Install for every supported agent
nxv skill install copilot --dir ~/repo   # Project install into another directory
nxv skill list                           # Show agents, paths, install status
nxv skill show                           # Print the SKILL.md to stdout
nxv skill uninstall --project            # Remove project-level installs
```

Supported agents and where the skill lands (`<dir>/nxv/SKILL.md`):

| Agent      | User-wide                 | Project-level     |
| ---------- | ------------------------- | ----------------- |
| `claude`   | `~/.claude/skills/`       | `.claude/skills/` |
| `codex`    | `~/.codex/skills/`        | `.agents/skills/` |
| `pi`       | `~/.pi/agent/skills/`     | `.pi/skills/`     |
| `openclaw` | `~/.openclaw/skills/`     | `.agents/skills/` |
| `copilot`  | `~/.copilot/skills/`      | `.github/skills/` |
| `cursor`   | `~/.cursor/skills/`       | `.agents/skills/` |
| `gemini`   | `~/.gemini/skills/`       | `.agents/skills/` |
| `amp`      | `~/.config/amp/skills/`   | `.agents/skills/` |
| `goose`    | `~/.config/goose/skills/` | `.agents/skills/` |
| `agents`   | `~/.agents/skills/`       | `.agents/skills/` |

The table shows each agent's primary directory — the one `nxv skill install <agent>` writes to. Several agents read additional locations: Copilot reads `.github/skills/`, `.claude/skills/`, or `.agents/skills/` in a repository, and Pi reads `.agents/skills/` as well as `.pi/skills/`.

Semantics:

- Installation requires one explicit target mode: one or more agent names,
  `--detected`, or `--all`. With no target, nxv exits without writing anything.
- `--detected` checks user configuration directories. With `--project` / `--dir`,
  those detected agents are mapped to their corresponding project paths. No
  detected agents is an error; use the explicit `agents` target for the generic
  Agent Skills directory.
- Agents sharing a directory (e.g. codex/cursor/gemini at project level) are deduplicated into a single write.
- Install overwrites `skills/nxv/SKILL.md` unconditionally and never touches other files; uninstall removes only that file (and the `nxv/` directory if it is then empty).

## Indexer Commands (feature-gated)

These require nxv built with `--features indexer` (`cargo build --features indexer` or `nix build .#nxv-indexer`). The indexer ingests channel-release snapshots from releases.nixos.org — no nixpkgs checkout and no Nix evaluation needed for the main path:

```bash
# Ingest new channel releases (default channels: nixpkgs-unstable + nixos-unstable-small)
nxv index                                            # Incremental: only new releases
nxv index --channel nixpkgs-unstable                 # Restrict to one channel
nxv index --since 2024-01-01 --until 2024-06-30      # Bound by release date
nxv index --strict --report report.json              # CI mode: gates fatal, JSON report
nxv index --backfill-evals                           # One-time 2016-2020 era (needs `nix`, ~2-3h)
nxv index --head-eval                                # Evaluate master HEAD when channels stall (needs `nix`)
nxv index --retry-failed                             # Re-attempt failed/parked releases
nxv index --max-releases 5                           # Bound a run (testing)

# Repair duplicate rows in pre-v4 databases (also runs during v3->v4 migration)
nxv dedupe --dry-run                                 # Preview
nxv dedupe                                           # Run

# Publish distribution-ready compressed artifacts + manifest
nxv publish --output ./publish --url-prefix https://your-server/nxv
nxv publish --output ./publish --url-prefix https://... --sign --secret-key nxv.key
nxv publish --output ./publish --url-prefix https://... --artifact-name-prefix run-123-

# Generate a minisign keypair for signing manifests
nxv keygen --secret-key ./nxv.key --public-key ./nxv.pub
```

Every recorded commit is a real, Hydra-built channel commit — `nix shell` commands produced from the index hit the binary cache instead of compiling from source. Version ranges mean "observed at both endpoints"; a version that lived shorter than one channel advance (~a day) may be missed.

Retired commands: `nxv backfill` and `nxv reset` are gone — snapshots carry complete metadata (source_path, homepage, known_vulnerabilities), and there is no checkout to reset.

Most users never need these — they consume a pre-built published index via `nxv sync`. Only run these when self-hosting an index. Use `--artifact-name-prefix` for mutable stores such as GitHub Releases so payload assets can be uploaded under immutable names before replacing `manifest.json`.

## Key Environment Variables

| Variable             | Purpose                                                                |
| -------------------- | ---------------------------------------------------------------------- |
| `NXV_API_URL`        | Point CLI at a remote `nxv serve` instead of the local DB              |
| `NXV_DB_PATH`        | Override local SQLite path                                             |
| `NXV_API_TIMEOUT`    | HTTP client timeout in seconds (default 30)                            |
| `NXV_MANIFEST_URL`   | Override the manifest URL used by `nxv sync`                           |
| `NXV_PUBLIC_KEY`     | Public key for manifest verification (path or raw key)                 |
| `NXV_SECRET_KEY`     | Secret key for `nxv publish --sign` (path or raw content)              |
| `NXV_SKIP_VERIFY`    | `1`/`true`/`yes`/`on` skip; `0`/`false`/`no`/`off` do not (INSECURE)   |
| `NXV_VERSION`        | Pin the version installed by `install.sh` (self-update targets latest) |
| `NXV_HOST`           | `nxv serve` bind host                                                  |
| `NXV_PORT`           | `nxv serve` listen port                                                |
| `NXV_RATE_LIMIT`     | `nxv serve` per-IP rate limit (req/sec)                                |
| `NXV_RATE_LIMIT_BURST` | `nxv serve` per-IP rate-limit burst size (default: 2x rate_limit)    |
| `NXV_FRONTEND_DIR`   | `nxv serve` reads frontend assets from disk on every request (dev mode)|
| `NO_COLOR`           | Disable ANSI colors when set to any non-empty value                    |

## Data Paths

The local index lives in platform data directories:

- **macOS**: `~/Library/Application Support/nxv/`
- **Linux**: `~/.local/share/nxv/`

Files:

- `index.db` — SQLite database with package versions
- `bloom.bin` — Bloom filter sibling for fast negative lookups (loaded at search time)

Safe to delete; `nxv sync` will rebuild from the published manifest.

## Agent Patterns

**Find the commit that introduced a specific version (CLI):**

```bash
nxv search python 3.11.4 --exact --format json | \
  jq '.[0] | {pkg: .attribute_path, version, commit: .first_commit_hash, date: .first_commit_date}'
```

**Generate the `nix shell` invocation directly (CLI):**

```bash
nxv search nodejs-15_x 15 --exact --format json | \
  jq -r '.[0] | "nix shell nixpkgs/\(.first_commit_hash | .[0:7])#\(.attribute_path)"'
```

**Find a package version on the public API:**

```bash
curl -s "https://nxv.urandom.io/api/v1/packages/python/versions/2.7.18/first" | \
  jq -r '.data | "nix shell nixpkgs/\(.first_commit_hash | .[0:7])#\(.attribute_path)"'
```

**Check whether a version of a package was ever in nixpkgs (HTTP):**

```bash
curl -s "https://nxv.urandom.io/api/v1/packages/ruby/history" | \
  jq '.data[] | select(.version | startswith("2.6"))'
```

**Get the index freshness (newest ingested channel commit):**

```bash
curl -s "https://nxv.urandom.io/api/v1/stats" | \
  jq '.data | {commit: .last_indexed_commit, date: .last_indexed_date, packages: .unique_names}'
```

## Key Invariants for Agents

- **Search is dedup-by-default**: by default, only the most recent record per `(attribute_path, version)` pair is returned. Pass `--full` (CLI) or use the `/packages/{attr}` HTTP endpoint to see every commit.
- **Version filters are prefix matches**: `nxv search python 3.11` matches `3.11.0`, `3.11.4`, `3.11.10`, etc. Use `--exact` for whole-attribute matches, not for whole-version matches.
- **Version searches use the closest attribute tier**: nested package-set members do not fill a direct-package miss. Inspect API `meta.resolution` or CLI stderr for suggestions, query the package-set prefix directly, or opt into `--all-depths` / `all_depths=true`.
- **Bloom filter gives instant negatives**: a search for a nonsense package name returns in <1 ms because the bloom filter rejects it before SQLite is touched. False positives are possible but rare.
- **Coverage starts in September 2016**: nixpkgs commits before then are not indexed. Anything older needs raw git spelunking.
- **Self-hosted indexes need a public key**: pass `--public-key` or set `NXV_PUBLIC_KEY` when consuming a manifest you signed yourself, otherwise `nxv sync` rejects the signature.
- **`--format json` shape is stable**: safe to pipe to `jq`. Breaking shape changes would be a semver bump — `license`/`maintainers`/`platforms`/`known_vulnerabilities` changed from stringified JSON to real arrays in a recent release.
- **Install with `attribute_path`, never `name`**: they differ often (`"name": "python3"` vs `"attribute_path": "python312"`). `name` is the upstream derivation name and is not a valid flake attribute on its own.
- **`/api/v1` data responses always wrap in `{data, meta}` (or `{data}` for single items)**: do `jq '.data'` first. Exceptions: the operational `/health` and `/metrics` endpoints are unwrapped.

## Practical Tips

- **Just want a python 2.7 shell?** Run `nxv run python 2.7`; nxv resolves the best match and handles old pre-flake revisions automatically.
- **Use `--exact`** when one exact attribute is required. Default version searches stay within the shallowest matching tier; use `--all-depths` only when nested variants are intentional.
- **Use `--desc`** for fuzzy intent ("a package that does X") instead of exact name searches.
- **Set `NXV_API_URL=https://nxv.urandom.io`** to skip the ~220MB index download entirely if you only need occasional lookups.
- **Sync regularly**: the public index is republished every 6 hours (`publish-index.yml`); `nxv sync` pulls the latest.
- **For CI**: run `nxv sync`; it never checks for or replaces the application.

## Updating This Skill

This skill is generated by the nxv binary itself. To refresh it after upgrading nxv:

```bash
nxv update                               # Get the latest nxv application
nxv skill install codex                  # Rewrite one user-wide install
nxv skill install --detected             # ...or explicitly refresh detected agents
nxv skill install codex --project        # Refresh one project-level install
```

Without an nxv binary on hand, fetch the canonical copy from the repository:

```bash
curl -sL https://raw.githubusercontent.com/utensils/nxv/main/.claude/skills/nxv/SKILL.md \
  -o ~/.claude/skills/nxv/SKILL.md
```
