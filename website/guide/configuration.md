# Configuration

nxv is configured through environment variables. All settings have sensible
defaults.

## Environment Variables

### Database

| Variable      | Description                       | Default           |
| ------------- | --------------------------------- | ----------------- |
| `NXV_DB_PATH` | Path to the SQLite index database | Platform data dir |

### Remote API

| Variable          | Description                              | Default |
| ----------------- | ---------------------------------------- | ------- |
| `NXV_API_URL`     | Use remote API instead of local database | None    |
| `NXV_API_TIMEOUT` | API request timeout in seconds           | `30`    |

When `NXV_API_URL` is set, nxv will query the remote server instead of the local
database. This is useful for CI/CD environments where you don't want to download
the index.

### Index Updates

| Variable           | Description                                                                                     | Default         |
| ------------------ | ----------------------------------------------------------------------------------------------- | --------------- |
| `NXV_MANIFEST_URL` | Custom manifest URL for index downloads                                                         | GitHub releases |
| `NXV_PUBLIC_KEY`   | Custom public key for manifest verification                                                     | Built-in key    |
| `NXV_SKIP_VERIFY`  | Skip manifest signature verification with `1`/`true`/`yes`/`on`; `0`/`false`/`no`/`off` keep verification enabled | `false`         |

These index variables apply to `nxv sync`. `nxv update` only updates the
application and does not read index configuration.

### Server

| Variable                 | Description                                                    | Default     |
| ------------------------ | -------------------------------------------------------------- | ----------- |
| `NXV_HOST`               | Server bind address                                            | `127.0.0.1` |
| `NXV_PORT`               | Server listen port                                             | `8080`      |
| `NXV_RATE_LIMIT`         | Rate limit per IP (requests/sec)                               | None        |
| `NXV_RATE_LIMIT_BURST`   | Rate limit burst size                                          | `2x rate`   |
| `NXV_MAX_DB_CONNECTIONS` | Maximum concurrent database operations                         | `32`        |
| `NXV_DB_TIMEOUT_SECS`    | Per-operation database timeout in seconds                      | `30`        |
| `NXV_LOG_FORMAT`         | Set to `json` for structured logs (combine with `RUST_LOG`)    | Plain text  |
| `NXV_FRONTEND_DIR`       | Serve frontend assets from this directory (disables 24h cache) | Embedded    |

`NXV_FRONTEND_DIR` is intended for development: set it to the checkout's
`frontend/` directory and edits to `index.html`, `app.js`, or `favicon.svg` are
picked up on the next request without rebuilding. Leave unset in production to
serve the embedded copy with a 24h `Cache-Control`.

### Indexing & Publishing

These only apply to builds with the `indexer` feature:

| Variable           | Description                                              | Default              |
| ------------------ | -------------------------------------------------------- | -------------------- |
| `NXV_SECRET_KEY`   | Minisign secret key (path or contents) for `nxv publish` | None                 |
| `NXV_RELEASES_URL` | Override the releases.nixos.org endpoint for `nxv index` | `releases.nixos.org` |

### Output

| Variable   | Description                                            | Default |
| ---------- | ------------------------------------------------------ | ------- |
| `NO_COLOR` | Disable colored output when set to any non-empty value | Not set |

## Data Directories

nxv stores data in platform-specific directories:

| Platform | Path                                 |
| -------- | ------------------------------------ |
| Linux    | `~/.local/share/nxv/`                |
| macOS    | `~/Library/Application Support/nxv/` |

Files stored:

- `index.db` - SQLite database with package versions
- `bloom.bin` - Bloom filter for fast negative lookups

## Example Configurations

### CI/CD (Remote API)

```bash
export NXV_API_URL="https://nxv.example.com"
nxv search python --version 3.11
```

### Custom Index Location

```bash
export NXV_DB_PATH="/data/nxv/index.db"
nxv sync
```
