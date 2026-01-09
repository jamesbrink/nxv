---
name: nixhub-api
description: Query the NixHub (search.devbox.sh) API for Nix package versions, store paths, and commit hashes. Use when looking up package information from NixHub, comparing nxv data against NixHub, finding flake installables, or checking binary cache availability.
allowed-tools: Bash, Read
---

# NixHub API Skill

Query the NixHub API (search.devbox.sh) for Nix package information.

## API Base URL

```
https://search.devbox.sh
```

Web UI: https://www.nixhub.io/

## Common Operations

### Search for packages

```bash
curl -s "https://search.devbox.sh/v2/search?q=python" | jq '.results[:5]'
```

### Get all versions of a package

```bash
curl -s "https://search.devbox.sh/v2/pkg?name=hello" | jq '.'
```

### Resolve a specific version

```bash
curl -s "https://search.devbox.sh/v2/resolve?name=python&version=3.11" | jq '.'
```

### Get store path for x86_64-linux

```bash
curl -s "https://search.devbox.sh/v2/resolve?name=hello&version=latest" | \
  jq -r '.systems["x86_64-linux"].outputs[] | select(.default) | .path'
```

### Get flake installable reference

```bash
curl -s "https://search.devbox.sh/v2/resolve?name=hello&version=latest" | \
  jq -r '.systems["x86_64-linux"].flake_installable | "github:\(.ref.owner)/\(.ref.repo)/\(.ref.rev)#\(.attr_path)"'
```

## API Endpoints

### Search (GET /v2/search)

Search packages by name or description.

```bash
curl -s "https://search.devbox.sh/v2/search?q=<query>"
```

**Response fields:**
- `query`: Search term
- `total_results`: Number of matches
- `results[].name`: Package name
- `results[].summary`: Package description
- `results[].last_updated`: Last update timestamp

### Package Info (GET /v2/pkg)

Get full package information with all versions.

```bash
curl -s "https://search.devbox.sh/v2/pkg?name=<package>"
```

**Response fields:**
- `name`: Package name
- `summary`: Package description
- `homepage_url`: Project homepage
- `license`: License identifier
- `releases[]`: Array of versions with:
  - `version`: Version string
  - `last_updated`: Timestamp
  - `platforms[]`: Per-system info with:
    - `system`: e.g., "x86_64-linux"
    - `attribute_path`: Nix attribute
    - `commit_hash`: nixpkgs commit
    - `outputs[].path`: Store path

### Resolve (GET /v2/resolve)

Resolve a version constraint to a specific version.

```bash
curl -s "https://search.devbox.sh/v2/resolve?name=<package>&version=<version>"
```

**Version formats:**
- `latest` - Latest version
- `3.11` - Specific version or prefix
- `3.x` - Major version constraint

**Response fields:**
- `name`, `version`, `summary`
- `systems.<system>.flake_installable`: Flake reference
- `systems.<system>.outputs[]`: Store paths

## Useful Queries

### List all versions of a package

```bash
curl -s "https://search.devbox.sh/v2/pkg?name=nodejs" | \
  jq -r '.releases[].version'
```

### Get version count

```bash
curl -s "https://search.devbox.sh/v2/pkg?name=firefox" | \
  jq '.releases | length'
```

### Get commit hash for a version

```bash
curl -s "https://search.devbox.sh/v2/pkg?name=hello" | \
  jq -r '.releases[] | select(.version == "2.12.1") | .platforms[] | select(.system == "x86_64-linux") | .commit_hash'
```

### Get versions with dates

```bash
curl -s "https://search.devbox.sh/v2/pkg?name=git" | \
  jq -r '.releases[] | "\(.version)\t\(.last_updated)"' | head -20
```

### Check if package exists

```bash
curl -s -o /dev/null -w "%{http_code}" "https://search.devbox.sh/v2/pkg?name=nonexistent"
# Returns 200 with empty releases if not found
```

## Binary Cache Verification

Check if a store path is available in cache.nixos.org:

```bash
# Extract hash from store path
STORE_PATH="/nix/store/2bcv91i8fahqghn8dmyr791iaycbsjdd-hello-2.12.2"
HASH=$(echo "$STORE_PATH" | sed 's|/nix/store/\([^-]*\)-.*|\1|')

# Check cache
curl -s -o /dev/null -w "%{http_code}" "https://cache.nixos.org/${HASH}.narinfo"
# 200 = available, 404 = not in cache
```

### Get narinfo details

```bash
HASH="2bcv91i8fahqghn8dmyr791iaycbsjdd"
curl -s "https://cache.nixos.org/${HASH}.narinfo"
```

## Using with fetchClosure

Instant binary download without evaluating nixpkgs:

```bash
# Get store path from NixHub
STORE_PATH=$(curl -s "https://search.devbox.sh/v2/resolve?name=hello&version=latest" | \
  jq -r '.systems["x86_64-linux"].outputs[] | select(.default) | .path')

# Build with fetchClosure
nix build --expr "builtins.fetchClosure { fromStore = \"https://cache.nixos.org\"; fromPath = $STORE_PATH; }"
```

## Comparing NXV with NixHub

### Quick version count comparison

```bash
PACKAGE="firefox"
DB_PATH="${XDG_DATA_HOME:-$HOME/.local/share}/nxv/index.db"

# NXV count
NXV_COUNT=$(sqlite3 "$DB_PATH" "SELECT COUNT(DISTINCT version) FROM package_versions WHERE attribute_path = '$PACKAGE'")

# NixHub count
NIXHUB_COUNT=$(curl -s "https://search.devbox.sh/v2/pkg?name=$PACKAGE" | jq '.releases | length')

echo "NXV: $NXV_COUNT versions, NixHub: $NIXHUB_COUNT versions"
```

### Run full validation script

```bash
python scripts/validate_against_nixhub.py --packages hello,firefox,git --verbose
```

### Comprehensive validation

```bash
python scripts/validate_against_nixhub.py --comprehensive --before 2023-07-25
```

## Rate Limiting

The API has rate limits. Add delays between requests:

```bash
for pkg in hello git curl; do
  curl -s "https://search.devbox.sh/v2/pkg?name=$pkg" | jq '.releases | length'
  sleep 0.5
done
```

## See Also

- `docs/nixhub-api.md` - Full API documentation
- `scripts/validate_against_nixhub.py` - Validation script
- Web UI: https://www.nixhub.io/
