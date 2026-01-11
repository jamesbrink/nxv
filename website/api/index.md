# HTTP API

nxv includes a REST API server for programmatic access.

## Public Instance

A public instance is available for testing and light usage:

- **Web UI:** [nxv.urandom.io](https://nxv.urandom.io/)
- **API Docs:** [nxv.urandom.io/docs](https://nxv.urandom.io/docs)
- **API Base:** `https://nxv.urandom.io/api/v1`

::: tip Try it now You can use the public API directly without setting up your
own server:

```bash
curl "https://nxv.urandom.io/api/v1/search?q=python&limit=5"
```

:::

## Self-Hosted

To run your own instance, start the server with `nxv serve`.

### Base URL

```
http://localhost:8080/api/v1
```

## Authentication

No authentication required. Rate limiting is applied per IP address.

## Endpoints

### Search Packages

```
GET /api/v1/search
```

**Query Parameters:**

| Parameter  | Type    | Description                                |
| ---------- | ------- | ------------------------------------------ |
| `q`        | string  | Search query (required)                    |
| `version`  | string  | Version filter (prefix match)              |
| `exact`    | boolean | Exact name match                           |
| `desc`     | boolean | Search descriptions                        |
| `license`  | string  | License filter                             |
| `platform` | string  | Platform filter                            |
| `sort`     | string  | Sort order: relevance, date, version, name |
| `reverse`  | boolean | Reverse sort                               |
| `limit`    | integer | Max results (default: 20, max: 100)        |
| `offset`   | integer | Pagination offset                          |

**Example:**

```bash
curl "http://localhost:8080/api/v1/search?q=python&version=3.11&limit=5"
```

**Response:**

```json
{
  "results": [
    {
      "attribute_path": "python311",
      "name": "python311",
      "version": "3.11.4",
      "description": "A high-level dynamically-typed programming language",
      "license": "Python-2.0",
      "first_commit_hash": "abc123...",
      "first_commit_date": "2023-06-15T00:00:00Z",
      "last_commit_hash": "def456...",
      "last_commit_date": "2023-12-01T00:00:00Z"
    }
  ],
  "total": 42,
  "limit": 5,
  "offset": 0
}
```

### Get Package Info

```
GET /api/v1/packages/{attribute_path}
```

Returns the latest version of a package.

**Example:**

```bash
curl "http://localhost:8080/api/v1/packages/python311"
```

### Get Specific Version

```
GET /api/v1/packages/{attribute_path}/versions/{version}/first
GET /api/v1/packages/{attribute_path}/versions/{version}/last
```

Get the first or last occurrence of a specific version.

**Example:**

```bash
curl "http://localhost:8080/api/v1/packages/python311/versions/3.11.4/first"
```

### Version History

```
GET /api/v1/history/{attribute_path}
```

**Query Parameters:**

| Parameter | Type    | Description                |
| --------- | ------- | -------------------------- |
| `limit`   | integer | Max versions (default: 50) |

**Example:**

```bash
curl "http://localhost:8080/api/v1/history/python311?limit=10"
```

**Response:**

```json
{
  "attribute_path": "python311",
  "versions": [
    {
      "version": "3.11.4",
      "first_commit_date": "2023-06-15T00:00:00Z",
      "last_commit_date": "2023-12-01T00:00:00Z"
    },
    {
      "version": "3.11.3",
      "first_commit_date": "2023-04-05T00:00:00Z",
      "last_commit_date": "2023-06-14T00:00:00Z"
    }
  ]
}
```

### Index Statistics

```
GET /api/v1/stats
```

**Response:**

```json
{
  "total_packages": 95000,
  "total_versions": 2800000,
  "index_commit": "abc123...",
  "index_date": "2024-01-15T00:00:00Z",
  "schema_version": 4
}
```

### Health Check

```
GET /api/v1/health
```

**Response:**

```json
{
  "status": "ok"
}
```

## Error Responses

All errors return a JSON object:

```json
{
  "error": "Package not found",
  "code": "NOT_FOUND"
}
```

**HTTP Status Codes:**

| Code | Description                      |
| ---- | -------------------------------- |
| 200  | Success                          |
| 400  | Bad request (invalid parameters) |
| 404  | Not found                        |
| 429  | Rate limited                     |
| 500  | Internal server error            |

## OpenAPI Documentation

Interactive API documentation is available at:

```
http://localhost:8080/docs
```

## CORS

Enable CORS for browser access:

```bash
# All origins
nxv serve --cors

# Specific origins
nxv serve --cors-origins "https://example.com,https://app.example.com"
```

## Request Headers

| Header         | Description                                                 |
| -------------- | ----------------------------------------------------------- |
| `X-Request-ID` | Correlation ID for tracing (auto-generated if not provided) |

The server echoes back the request ID in responses for distributed tracing.
