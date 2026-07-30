# CLI Reference

Complete documentation for all nxv commands and options.

## Global Options

| Flag                   | Description                                             |
| ---------------------- | ------------------------------------------------------- |
| `--db-path <DB_PATH>`  | Path to the index database (default: platform data dir) |
| `-v, --verbose...`     | Enable verbose output (`-v` info, `-vv` debug)          |
| `-q, --quiet`          | Suppress all output except errors                       |
| `--no-color`           | Disable colored output (also: `NO_COLOR` env)           |
| `--api-timeout <SECS>` | API request timeout in seconds (default: 30)            |
| `-h, --help`           | Print help                                              |
| `-V, --version`        | Print version                                           |

## Commands

### search

Search for packages by name, version, or description.

```bash
nxv search <PACKAGE> [VERSION]
```

**Options:**

| Flag                      | Description                                          |
| ------------------------- | ---------------------------------------------------- |
| `-V, --version <VERSION>` | Filter by version (alternative to positional)        |
| `-e, --exact`             | Exact name match only                                |
| `--all-depths`            | Include nested attribute depths in a version search  |
| `--desc`                  | Search in descriptions (full-text)                   |
| `--license <LICENSE>`     | Filter by license                                    |
| `--show-platforms`        | Show platforms column                                |
| `--sort <ORDER>`          | Sort order: relevance (default), date, version, name |
| `-r, --reverse`           | Reverse sort order                                   |
| `-n, --limit <N>`         | Maximum results (default: 50, 0=unlimited)           |
| `--full`                  | Show all commits (no deduplication)                  |
| `--ascii`                 | ASCII table borders                                  |
| `-f, --format <FORMAT>`   | Output format: table, json, plain                    |

**Examples:**

```bash
# Basic search
nxv search python

# Find specific version
nxv search python 3.11.4

# Intentionally include nested package-set members with that version
nxv search python 2.7.3 --all-depths

# Search descriptions
nxv search "web server" --desc

# JSON output for scripts
nxv search python --format json

# Sort by date, newest first
nxv search python --sort date
```

**Version matching:** without `--exact`, nxv first resolves the shallowest
attribute-path tier matching the package prefix and then applies the version
prefix. This keeps `python 3.11` on interpreter attributes such as `python311`,
and makes `python 2.7.3` a precise miss rather than returning libraries whose
own version is 2.7.3. A package-set prefix such as `python27Packages 2.7.3`
naturally resolves its member depth. Use `--all-depths` to request the legacy
broad prefix behavior explicitly; it requires a version and cannot be combined
with `--exact` or `--desc`.

On a version miss, table/plain searches print nearby attribute/version
suggestions and indicate when deeper matches are available. Successful JSON
searches keep returning an array of package rows; an empty miss emits no stdout.
Diagnostics stay on stderr in either case.

### run

Resolve indexed package versions and open one interactive shell containing
them.

```bash
nxv run <PACKAGE> [VERSION] [options]
```

**Options:**

| Flag                         | Description                                   |
| ---------------------------- | --------------------------------------------- |
| `-V, --version <VERSION>`    | Primary version (alternative to positional)   |
| `-e, --exact`                | Match exact attribute paths for every package |
| `--all-depths`               | Include nested depths in version searches     |
| `--with <PACKAGE[@VERSION]>` | Add another package; may be repeated           |

**Examples:**

```bash
# Open a shell with the best Python 2.7 match
nxv run python 2.7

# Combine independently pinned package versions
nxv run python 3.11 --with nodejs@20 --with jq

# Require exact attribute paths
nxv run python311 3.11 --exact --with nodejs@20
```

Each query prefers an exact attribute match, then falls back to the same
relevance and shallowest-depth rules as [`search`](#search). nxv resolves every
package before launching anything, selects the first deterministic match, and
pins its latest observed commit. Modern revisions are passed as separate
installables to one `nix shell` process. If any selected revision predates
flakes, nxv instead creates one compatible `nix-shell -p` environment using
pinned `fetchTarball` expressions. On Apple Silicon, pre-flake environments are
evaluated as `x86_64-darwin` and require Rosetta.

Known-insecure selections are called out before launch and automatically receive
the Nix setting needed to evaluate them. If any query misses, no shell starts.

### info

Show detailed information about a specific package version.

```bash
nxv info <package> [version] [options]
```

**Options:**

| Flag                      | Description                                  |
| ------------------------- | -------------------------------------------- |
| `-V, --version <VERSION>` | Specific version (alternative to positional) |
| `-f, --format <FORMAT>`   | Output format: table, json, plain            |

**Matching:** `info` resolves `<package>` as an **exact attribute path** first,
so it needs no `--exact` flag — `nxv info python311 3.11.4` returns `python311`
only, never `python311Full` or `python311Packages.*`. If the package is known
but never had the requested version, `info` reports not found rather than
falling back to unrelated prefix matches.

An unknown attribute path is widened to a prefix search, but what gets
prefix-matched differs by invocation: **with** a version it matches attribute
paths, so `nxv info python311Packages.req 2.32` resolves; **without** a version
it matches the package `name` field instead (values like `python-3.11.0`), so
partial attribute paths generally do not resolve unversioned. Use
[`search`](#search) for open-ended prefix matching.

**Examples:**

```bash
# Latest version info
nxv info python311

# Specific version — exact package, no prefix contamination
nxv info python311 3.11.4

# Partial attribute path resolves when a version is supplied
nxv info python311Packages.req 2.32
```

### history

Show version history for a package.

```bash
nxv history <PACKAGE> [VERSION]
```

**Options:**

| Flag                    | Description                       |
| ----------------------- | --------------------------------- |
| `-f, --format <FORMAT>` | Output format: table, json, plain |
| `--full`                | Show full details                 |
| `--ascii`               | ASCII table borders               |

**Examples:**

```bash
# Full history
nxv history python311
```

### update

Update the nxv application to the latest release. This command never downloads
or modifies the package index.

```bash
nxv update
```

**Binary-update behaviour by install method:**

| Install method  | Action                                                             |
| --------------- | ------------------------------------------------------------------ |
| Local           | Downloads, verifies SHA-256, atomically swaps the binary           |
| Nix             | Leaves binary alone; prints `nix profile upgrade nxv` / flake hint |
| `cargo install` | Leaves binary alone; prints `cargo install --locked nxv`           |
| Homebrew        | Leaves binary alone; prints `brew upgrade nxv` (or reinstall)      |

Set `GITHUB_TOKEN` to avoid unauthenticated rate limits when calling the GitHub
API. If GitHub rejects the token (401 — e.g. a stale token exported by a dev
shell), the check is retried without it. Release lookup, download, verification,
or replacement failures cause the command to fail.

Legacy index flags are rejected with guidance to use `nxv sync`. The retired
`NXV_NO_SELF_UPDATE` variable is likewise rejected when truthy so an old
configuration cannot unexpectedly replace the application.

### sync

Download or refresh the local package index without checking for application
updates.

```bash
nxv sync [options]
```

**Options:**

| Flag                   | Description                          |
| ---------------------- | ------------------------------------ |
| `-f, --force`          | Force full re-download of the index  |
| `--manifest-url <URL>` | Custom or self-hosted index manifest |
| `--skip-verify`        | Skip manifest signature verification |
| `--public-key <KEY>`   | Custom public key for verification   |

```bash
nxv sync
nxv sync --force
nxv sync --manifest-url https://example.com/manifest.json --public-key ./nxv.pub
```

`sync` is safe for CI and timers because it never updates the application. If
the published index requires a newer nxv schema, run `nxv update` (or the
printed managed-install command) and retry `nxv sync`.

### serve

Start the HTTP API server.

```bash
nxv serve [options]
```

**Options:**

| Flag                       | Description                       |
| -------------------------- | --------------------------------- |
| `-H, --host <HOST>`        | Host address (default: 127.0.0.1) |
| `-p, --port <PORT>`        | Listen port (default: 8080)       |
| `--cors`                   | Enable CORS for all origins       |
| `--cors-origins <ORIGINS>` | Specific CORS origins             |
| `--rate-limit <N>`         | Rate limiting per IP (req/sec)    |
| `--rate-limit-burst <N>`   | Burst size for rate limiting      |

`--host`, `--port`, `--rate-limit`, and `--rate-limit-burst` are also settable
via `NXV_HOST`, `NXV_PORT`, `NXV_RATE_LIMIT`, and `NXV_RATE_LIMIT_BURST` — see
[Configuration](/guide/configuration).

**Examples:**

```bash
# Default (localhost:8080)
nxv serve

# Public server with CORS
nxv serve --host 0.0.0.0 --port 3000 --cors
```

### stats

Show index statistics and metadata.

```bash
nxv stats
```

### index

Build the index from nixpkgs channel-release snapshots on releases.nixos.org.
Downloads and streams `packages.json.br` for each release — no nixpkgs checkout
and no Nix evaluation needed, except for the two flags noted below that require
`nix`.

Requires the `indexer` feature (`nxv-indexer` or
`cargo build --features indexer`).

```bash
nxv index [options]
```

**Options:**

| Flag                   | Description                                                                                          |
| ---------------------- | ---------------------------------------------------------------------------------------------------- |
| `--channel <CHANNELS>` | Channels to ingest (repeatable or comma-separated; default: nixpkgs-unstable + nixos-unstable-small) |
| `--since <DATE>`       | Only ingest releases dated on/after this date (YYYY-MM-DD)                                           |
| `--until <DATE>`       | Only ingest releases dated on/before this date (YYYY-MM-DD)                                          |
| `--jobs <N>`           | Parallel snapshot download/parse workers                                                             |
| `--strict`             | Treat monitor warnings (count floors, sentinels, head lag) as fatal                                  |
| `--report <PATH>`      | Write the end-of-run coverage report as JSON to this path                                            |
| `--retry-failed`       | Retry releases that were parked as failed/skipped                                                    |
| `--backfill-evals`     | Also ingest the pre-2020 era by evaluating `nixexprs.tar.xz` with nix-env (requires `nix`)           |
| `--head-eval`          | Evaluate nixpkgs master HEAD when channel observations lag behind (requires `nix`)                   |
| `--full`               | Re-plan every known release instead of only new ones                                                 |
| `--max-releases <N>`   | Limit the number of releases ingested this run (for testing)                                         |

### dedupe

Collapse duplicate `(attribute_path, version)` rows in the local index. Repairs
databases bloated by the pre-0.1.5 incremental indexer bug. Keeps one row per
unique pair with the earliest `first_commit_*` and the latest `last_commit_*`
across the duplicates, then `VACUUM`s.

Requires the `indexer` feature (`nxv-indexer` or
`cargo build --features indexer`).

```bash
nxv dedupe [options]
```

**Options:**

| Flag          | Description                                               |
| ------------- | --------------------------------------------------------- |
| `--dry-run`   | Report what would change without modifying the database   |
| `--no-vacuum` | Skip the trailing `VACUUM` (faster, DB file won't shrink) |

### publish

Generate publishable index artifacts (compressed database, bloom filter, and
manifest) for distribution.

Requires the `indexer` feature.

```bash
nxv publish [options]
```

**Options:**

| Flag                              | Description                                                                                                                                                                |
| --------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `-o, --output <DIR>`              | Output directory for generated artifacts (default: ./publish)                                                                                                              |
| `--url-prefix <URL>`              | Base URL prefix for manifest URLs                                                                                                                                          |
| `--artifact-name-prefix <PREFIX>` | Prefix added to index/bloom artifact filenames in the manifest (useful when uploading immutable, run-specific artifact names while manifest.json stays the stable pointer) |
| `--sign`                          | Sign the manifest with a minisign secret key                                                                                                                               |
| `--secret-key <KEY>`              | Secret key for signing (file path or raw key; also `NXV_SECRET_KEY` env)                                                                                                   |
| `--min-version <N>`               | Minimum schema version required to read this index (default: the schema version)                                                                                           |

### keygen

Generate a new minisign keypair for signing manifests.

Requires the `indexer` feature.

```bash
nxv keygen [options]
```

**Options:**

| Flag                      | Description                                                  |
| ------------------------- | ------------------------------------------------------------ |
| `-s, --secret-key <PATH>` | Output path for the secret key file (default: ./nxv.key)     |
| `-p, --public-key <PATH>` | Output path for the public key file (default: ./nxv.pub)     |
| `-c, --comment <COMMENT>` | Comment to embed in the key files (default: nxv signing key) |
| `-f, --force`             | Overwrite existing key files if they exist                   |

### completions

Generate shell completion scripts.

```bash
nxv completions <shell>
```

**Shells:** bash, zsh, fish, powershell, elvish

**Examples:**

```bash
# Bash
nxv completions bash > ~/.local/share/bash-completion/completions/nxv

# Zsh
nxv completions zsh > ~/.zfunc/_nxv

# Fish
nxv completions fish > ~/.config/fish/completions/nxv.fish
```

### skill

Generate and install the nxv agent skill (a `SKILL.md` following the
[Agent Skills](https://agentskills.io) standard) so AI coding agents can drive
nxv. See the [Agent Skill guide](/guide/skill) for details.

```bash
nxv skill <list|install|uninstall|show>
```

**Subcommands:**

| Subcommand              | Description                                            |
| ----------------------- | ------------------------------------------------------ |
| `list`                  | List supported agents, skill paths, and install status |
| `install [AGENTS...]`   | Install the skill (user-wide by default)               |
| `uninstall [AGENTS...]` | Remove installed skills                                |
| `show`                  | Print the generated SKILL.md to stdout                 |

**Agents:** claude, codex, pi, openclaw, copilot, cursor, gemini, amp, goose,
agents (the generic cross-agent `.agents/skills` directory)

**Install/uninstall options:**

| Flag           | Description                                               |
| -------------- | --------------------------------------------------------- |
| `--project`    | Use project-level skill directories under the current dir |
| `--dir <PATH>` | Project directory to operate on (implies `--project`)     |
| `--detected`   | Install for agents detected from user configuration       |
| `--all`        | Install for all supported agents (install only)           |

**List options:**

| Flag      | Description                                |
| --------- | ------------------------------------------ |
| `--ascii` | Use ASCII table borders instead of Unicode |

**Examples:**

```bash
# Install user-wide for one agent
nxv skill install codex

# Explicitly install for detected agents
nxv skill install --detected

# Map detected agents to their project paths
nxv skill install --detected --project

# See where the skill is (or would be) installed
nxv skill list
```

Install requires agent names, `--detected`, or `--all`; supplying no target is
an error and writes nothing. Shared project paths are deduplicated.

## Output Formats

### table (default)

Human-readable table with colors:

```
Package          Version   Commit    Date         Description
python311        3.11.4    abc1234   2023-06-15   High-level dynamically-typed programming language
python311        3.11.3    def5678   2023-04-05   High-level dynamically-typed programming language
```

### json

Machine-readable JSON (abbreviated below):

```json
[
  {
    "attribute_path": "python311",
    "version": "3.11.4",
    "first_commit_date": "2023-06-15T00:00:00Z",
    "first_commit_hash": "abc1234..."
  }
]
```

Each row also carries `id`, `name`, `last_commit_hash`, `last_commit_date`,
`description`, `license`, `homepage`, `maintainers`, `platforms`, `source_path`,
and `known_vulnerabilities` (array or null where applicable).

### plain

Tab-separated values for scripts (with a header row):

```
PACKAGE	VERSION	COMMIT	DATE	DESCRIPTION
python311	3.11.4	abc1234	2023-06-15	High-level dynamically-typed programming language
python311	3.11.3	def5678	2023-04-05	High-level dynamically-typed programming language
```
