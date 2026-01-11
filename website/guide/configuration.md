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

| Variable           | Description                                 | Default         |
| ------------------ | ------------------------------------------- | --------------- |
| `NXV_MANIFEST_URL` | Custom manifest URL for index downloads     | GitHub releases |
| `NXV_PUBLIC_KEY`   | Custom public key for manifest verification | Built-in key    |
| `NXV_SKIP_VERIFY`  | Skip manifest signature verification        | `false`         |

### Logging

| Variable           | Description                                | Default  |
| ------------------ | ------------------------------------------ | -------- |
| `NXV_LOG`          | Log filter (overrides RUST_LOG)            | None     |
| `NXV_LOG_LEVEL`    | Log level: error, warn, info, debug, trace | `warn`   |
| `NXV_LOG_FORMAT`   | Output format: pretty, compact, json       | `pretty` |
| `NXV_LOG_FILE`     | Path to log file (in addition to stderr)   | None     |
| `NXV_LOG_ROTATION` | Log file rotation: hourly, daily, never    | `daily`  |

### Output

| Variable   | Description            | Default |
| ---------- | ---------------------- | ------- |
| `NO_COLOR` | Disable colored output | Not set |

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

### Debug Logging

```bash
export NXV_LOG_LEVEL=debug
export NXV_LOG_FORMAT=json
nxv search python
```

### Custom Index Location

```bash
export NXV_DB_PATH="/data/nxv/index.db"
nxv update
```
