# CLI Reference

Complete documentation for all nxv commands and options.

## Global Options

```
-h, --help     Print help
-V, --version  Print version
```

## Commands

### search

Search for packages by name, version, or description.

```bash
nxv search <query> [options]
```

**Options:**

| Flag                      | Description                                |
| ------------------------- | ------------------------------------------ |
| `-v, --version <VERSION>` | Filter by version (prefix match)           |
| `-e, --exact`             | Exact name match only                      |
| `-d, --desc`              | Search in descriptions (full-text)         |
| `--license <LICENSE>`     | Filter by license                          |
| `--platform <PLATFORM>`   | Filter by platform                         |
| `-s, --sort <ORDER>`      | Sort order: relevance, date, version, name |
| `-r, --reverse`           | Reverse sort order                         |
| `-l, --limit <N>`         | Maximum results (default: 20)              |
| `--offset <N>`            | Skip first N results                       |
| `-f, --full`              | Show all commits (no deduplication)        |
| `-o, --output <FORMAT>`   | Output format: table, json, plain          |

**Examples:**

```bash
# Basic search
nxv search python

# Find specific version
nxv search python --version 3.11.4

# Search descriptions
nxv search "web server" --desc

# JSON output for scripts
nxv search python --output json

# Sort by date, newest first
nxv search python --sort date
```

### info

Show detailed information about a specific package version.

```bash
nxv info <package> [version] [options]
```

**Options:**

| Flag                    | Description                            |
| ----------------------- | -------------------------------------- |
| `--first`               | Show first occurrence (oldest)         |
| `--last`                | Show last occurrence (newest, default) |
| `-o, --output <FORMAT>` | Output format: table, json, plain      |

**Examples:**

```bash
# Latest version info
nxv info python311

# Specific version
nxv info python311 3.11.4

# First appearance
nxv info python311 --first
```

### history

Show version history for a package.

```bash
nxv history <package> [options]
```

**Options:**

| Flag                    | Description                       |
| ----------------------- | --------------------------------- |
| `-l, --limit <N>`       | Maximum versions to show          |
| `-o, --output <FORMAT>` | Output format: table, json, plain |

**Examples:**

```bash
# Full history
nxv history python311

# Last 5 versions
nxv history python311 --limit 5
```

### update

Download or update the package index.

```bash
nxv update [options]
```

**Options:**

| Flag          | Description                      |
| ------------- | -------------------------------- |
| `--force`     | Force full download (skip delta) |
| `-q, --quiet` | Suppress progress output         |

### serve

Start the HTTP API server.

```bash
nxv serve [options]
```

**Options:**

| Flag                       | Description                            |
| -------------------------- | -------------------------------------- |
| `-p, --port <PORT>`        | Listen port (default: 8080)            |
| `-b, --bind <ADDR>`        | Bind address (default: 127.0.0.1)      |
| `--cors`                   | Enable CORS for all origins            |
| `--cors-origins <ORIGINS>` | Allowed CORS origins (comma-separated) |

**Examples:**

```bash
# Default (localhost:8080)
nxv serve

# Public server with CORS
nxv serve --bind 0.0.0.0 --port 3000 --cors
```

### stats

Show index statistics and metadata.

```bash
nxv stats [options]
```

**Options:**

| Flag                    | Description                       |
| ----------------------- | --------------------------------- |
| `-o, --output <FORMAT>` | Output format: table, json, plain |

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

## Output Formats

### table (default)

Human-readable table with colors:

```
Package          Version   Date         Commit
python311        3.11.4    2023-06-15   abc1234
python311        3.11.3    2023-04-05   def5678
```

### json

Machine-readable JSON:

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

### plain

Tab-separated values for scripts:

```
python311	3.11.4	2023-06-15	abc1234
python311	3.11.3	2023-04-05	def5678
```
