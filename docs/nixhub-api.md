# NixHub / Devbox Search API Documentation

> **Base URL:** `https://search.devbox.sh`
>
> **Web UI:** https://www.nixhub.io/
>
> **Maintained by:** [Jetify](https://www.jetify.com/) (creators of Devbox)

This is an unofficial documentation of the NixHub search API, reverse-engineered from the [Devbox client source code](https://github.com/jetify-com/devbox/tree/main/internal/searcher).

---

## Table of Contents

- [API Endpoints](#api-endpoints)
  - [Search](#search)
  - [Resolve](#resolve)
  - [Package Info](#package-info)
- [Version Syntax](#version-syntax)
- [Response Types](#response-types)
- [Using Store Paths with fetchClosure](#using-store-paths-with-fetchclosure)
- [Checking Binary Cache Availability](#checking-binary-cache-availability)

---

## API Endpoints

### Search

Search for packages by name or description.

#### `GET /v1/search`

```bash
curl "https://search.devbox.sh/v1/search?q=python"
```

#### `GET /v2/search`

```bash
curl "https://search.devbox.sh/v2/search?q=python"
```

**Parameters:**

| Param | Type   | Required | Description          |
|-------|--------|----------|----------------------|
| `q`   | string | Yes      | Search query string  |

**Response:**

```json
{
  "query": "hello",
  "total_results": 26,
  "results": [
    {
      "name": "hello",
      "summary": "Program that produces a familiar, friendly greeting",
      "last_updated": "2025-11-23T21:50:36Z"
    },
    {
      "name": "hello-cpp",
      "summary": "Basic sanity check that C++ and cmake infrastructure are working",
      "last_updated": "2025-11-23T21:50:36Z"
    }
  ]
}
```

---

### Resolve

Resolve a version constraint to a specific package version and nixpkgs commit.

#### `GET /v1/resolve`

```bash
curl "https://search.devbox.sh/v1/resolve?name=python&version=3.11"
```

**Response:** Returns `PackageVersion` type (see [Response Types](#response-types))

#### `GET /v2/resolve`

```bash
curl "https://search.devbox.sh/v2/resolve?name=python&version=3.11"
```

**Parameters:**

| Param     | Type   | Required | Description                          |
|-----------|--------|----------|--------------------------------------|
| `name`    | string | Yes      | Package name                         |
| `version` | string | Yes      | Version or semver constraint (e.g., `3.11`, `latest`) |

**Response:**

```json
{
  "name": "python",
  "version": "3.11.14",
  "summary": "High-level dynamically-typed programming language",
  "systems": {
    "x86_64-linux": {
      "flake_installable": {
        "ref": {
          "type": "github",
          "owner": "NixOS",
          "repo": "nixpkgs",
          "rev": "ee09932cedcef15aaf476f9343d1dea2cb77e261"
        },
        "attr_path": "python311"
      },
      "last_updated": "2025-11-23T21:50:36Z",
      "outputs": [
        {
          "name": "out",
          "path": "/nix/store/00x3abm7y8j13i6n4sahvbar99irkc7d-python3-3.11.14",
          "default": true
        },
        {
          "name": "debug",
          "path": "/nix/store/2lj58b7mhib83mb0cx8flyahbs9pnd9z-python3-3.11.14-debug"
        }
      ]
    },
    "aarch64-darwin": { "...": "..." },
    "aarch64-linux": { "...": "..." },
    "x86_64-darwin": { "...": "..." }
  }
}
```

The `flake_installable` maps directly to a Nix flake reference:

```
github:NixOS/nixpkgs/ee09932cedcef15aaf476f9343d1dea2cb77e261#python311
```

---

### Package Info

Get full package information including all available versions.

#### `GET /v1/pkg`

```bash
curl "https://search.devbox.sh/v1/pkg?name=hello"
```

**Response:** Returns an array of `PackageVersion` objects with detailed system-specific information including:

- `store_hash` - Nix store hash
- `store_name` - Package name in store
- `store_version` - Version string in store
- `programs` - List of executable names
- `description` - Full package description
- `attr_paths` - All attribute paths for this package

#### `GET /v2/pkg`

```bash
curl "https://search.devbox.sh/v2/pkg?name=hello"
```

**Parameters:**

| Param     | Type   | Required | Description                     |
|-----------|--------|----------|---------------------------------|
| `name`    | string | Yes      | Package name                    |
| `version` | string | No       | Optional version filter         |

**Response:**

```json
{
  "name": "hello",
  "summary": "Program that produces a familiar, friendly greeting",
  "homepage_url": "https://www.gnu.org/software/hello/manual/",
  "license": "GPL-3.0-or-later",
  "releases": [
    {
      "version": "2.12.2",
      "last_updated": "2025-11-23T21:50:36Z",
      "platforms": [
        {
          "arch": "x86-64",
          "os": "Linux",
          "system": "x86_64-linux",
          "attribute_path": "hello",
          "commit_hash": "ee09932cedcef15aaf476f9343d1dea2cb77e261",
          "date": "2025-11-23T21:50:36Z",
          "outputs": [
            {
              "name": "out",
              "path": "/nix/store/2bcv91i8fahqghn8dmyr791iaycbsjdd-hello-2.12.2",
              "default": true
            }
          ]
        }
      ],
      "platforms_summary": "Linux and macOS",
      "outputs_summary": ""
    },
    {
      "version": "2.12.1",
      "last_updated": "2025-05-16T20:19:48Z"
    }
  ]
}
```

---

## Version Syntax

The Devbox client parses versioned package strings using the `@` delimiter:

```
package@version
```

**Examples:**

| Input                    | Name               | Version  |
|--------------------------|--------------------|----------|
| `python@3.10`            | `python`           | `3.10`   |
| `python@latest`          | `python`           | `latest` |
| `nodejs@18.x`            | `nodejs`           | `18.x`   |
| `emacsPackages.@@latest` | `emacsPackages.@`  | `latest` |
| `emacsPackages.@`        | *(invalid)*        | -        |
| `python`                 | *(no version)*     | -        |

The parser uses `LastIndex("@")` to handle packages with `@` in their name.

---

## Response Types

### PackageVersion (v1)

```go
type PackageVersion struct {
    Name    string                 `json:"name"`
    Version string                 `json:"version"`
    Summary string                 `json:"summary"`
    Systems map[string]PackageInfo `json:"systems,omitempty"`
}

type PackageInfo struct {
    CommitHash   string   `json:"commit_hash"`
    System       string   `json:"system"`
    LastUpdated  int      `json:"last_updated"`
    StoreHash    string   `json:"store_hash"`
    StoreName    string   `json:"store_name"`
    StoreVersion string   `json:"store_version"`
    AttrPaths    []string `json:"attr_paths"`
    Programs     []string `json:"programs"`
    Summary      string   `json:"summary"`
    Description  string   `json:"description"`
    Homepage     string   `json:"homepage"`
    License      string   `json:"license"`
}
```

### ResolveResponse (v2)

```go
type ResolveResponse struct {
    Name    string `json:"name"`
    Version string `json:"version"`
    Summary string `json:"summary,omitempty"`
    Systems map[string]struct {
        FlakeInstallable struct {
            Ref struct {
                Type  string `json:"type"`
                Owner string `json:"owner"`
                Repo  string `json:"repo"`
                Rev   string `json:"rev"`
            } `json:"ref"`
            AttrPath string `json:"attr_path"`
        } `json:"flake_installable"`
        LastUpdated time.Time `json:"last_updated"`
        Outputs []struct {
            Name    string `json:"name,omitempty"`
            Path    string `json:"path,omitempty"`
            Default bool   `json:"default,omitempty"`
            NAR     string `json:"nar,omitempty"`
        } `json:"outputs,omitempty"`
    } `json:"systems"`
}
```

---

## Using Store Paths with fetchClosure

The API returns Nix store paths that can be used with `builtins.fetchClosure` for **instant binary downloads** without evaluating nixpkgs.

### The Problem: Traditional Approach is Slow

```nix
# Traditional: Download + decompress + evaluate nixpkgs (~30s+)
let
  pkgs = import (fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/ee09932cedcef15aaf476f9343d1dea2cb77e261.tar.gz";
  }) {};
in pkgs.hello
```

This requires:

1. Downloading ~300MB nixpkgs tarball
2. Decompressing it
3. Evaluating the Nix expression (10-30+ seconds)
4. Checking binary cache
5. Downloading binary (or building from source)

### The Solution: fetchClosure

```nix
# Fast: Direct binary fetch (~instant)
builtins.fetchClosure {
  fromStore = "https://cache.nixos.org";
  fromPath = /nix/store/2bcv91i8fahqghn8dmyr791iaycbsjdd-hello-2.12.2;
}
```

This skips nixpkgs evaluation entirely and pulls the pre-built binary directly.

### Example Workflow

1. **Query the API for a package:**

```bash
curl -s "https://search.devbox.sh/v2/resolve?name=hello&version=latest" | jq '.systems["x86_64-linux"].outputs[0].path'
# "/nix/store/2bcv91i8fahqghn8dmyr791iaycbsjdd-hello-2.12.2"
```

2. **Use in Nix:**

```nix
# flake.nix
{
  outputs = { self }: {
    packages.x86_64-linux.hello = builtins.fetchClosure {
      fromStore = "https://cache.nixos.org";
      fromPath = /nix/store/2bcv91i8fahqghn8dmyr791iaycbsjdd-hello-2.12.2;
    };
  };
}
```

3. **Or run directly:**

```bash
nix build --expr 'builtins.fetchClosure { fromStore = "https://cache.nixos.org"; fromPath = /nix/store/2bcv91i8fahqghn8dmyr791iaycbsjdd-hello-2.12.2; }'
./result/bin/hello
```

---

## Checking Binary Cache Availability

**Important:** Not all store paths are available in cache.nixos.org. Older or less popular packages may have been garbage collected.

### Check if a path exists in the cache

The store path `/nix/store/<hash>-<name>` can be checked via:

```bash
curl -I "https://cache.nixos.org/<hash>.narinfo"
```

**Example - Available (hello 2.12 from 2022):**

```bash
curl -s "https://cache.nixos.org/yspx4mzdrhsr37yy1yzd00vgv47wkizc.narinfo"
```

```
StorePath: /nix/store/yspx4mzdrhsr37yy1yzd00vgv47wkizc-hello-2.12
URL: nar/19sbi0ddhavyvak4iazxdg3cx0grnmr9lwwwpqgnnx4w48xzwabr.nar.xz
Compression: xz
FileHash: sha256:19sbi0ddhavyvak4iazxdg3cx0grnmr9lwwwpqgnnx4w48xzwabr
FileSize: 43604
NarHash: sha256:0f74si746iskxii4c3a34m72pwyd9wd9zgq8jnimvcpmmxmj94z5
NarSize: 181368
References: v483gzp7hv2hf5iz5sbbmv9qfhvninis-glibc-2.34-210 yspx4mzdrhsr37yy1yzd00vgv47wkizc-hello-2.12
Sig: cache.nixos.org-1:CklfdZtbfKnrLwDqC809MKryLzst4q1YphEGXDOuH63+...
```

**Example - Not Available (nodejs 14 from 2023):**

```bash
curl -I "https://cache.nixos.org/kynkkdchsgm650jmx794jjad7h7iifbi.narinfo"
# HTTP 404 - Not Found
```

### Cache Retention Guidelines

| Category | Typical Retention |
|----------|-------------------|
| Current nixos-unstable | Always available |
| Current stable channel (25.11) | Always available |
| Recent packages (~1-2 years) | Usually available |
| Popular packages | Longer retention |
| EOL versions (Node 14, Python 2, etc.) | Often garbage collected |
| Obscure packages | May be removed sooner |

### Fallback Strategy

```nix
# Try fetchClosure first, fall back to nixpkgs eval
let
  tryFetchClosure = builtins.tryEval (builtins.fetchClosure {
    fromStore = "https://cache.nixos.org";
    fromPath = /nix/store/2bcv91i8fahqghn8dmyr791iaycbsjdd-hello-2.12.2;
  });

  fromNixpkgs = (import (fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/ee09932...tar.gz";
  }) {}).hello;
in
  if tryFetchClosure.success then tryFetchClosure.value else fromNixpkgs
```

---

## API Summary

| Endpoint | Method | Params | Description |
|----------|--------|--------|-------------|
| `/v1/search` | GET | `q` | Search packages |
| `/v2/search` | GET | `q` | Search packages |
| `/v1/resolve` | GET | `name`, `version` | Resolve version -> commit + store path |
| `/v2/resolve` | GET | `name`, `version` | Resolve version -> flake installable |
| `/v1/pkg` | GET | `name` | Full package history (detailed) |
| `/v2/pkg` | GET | `name`, `version`? | Full package history (cleaner structure) |

---

## See Also

- [Devbox](https://github.com/jetify-com/devbox) - CLI tool that uses this API
- [NixHub Web UI](https://www.nixhub.io/) - Web interface for searching
- [lazamar/nix-package-versions](https://github.com/lazamar/nix-package-versions) - Open source alternative
- [Nix fetchClosure documentation](https://nixos.org/manual/nix/stable/language/builtins.html#builtins-fetchClosure)
