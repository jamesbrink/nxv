//! HTTP client for remote API access.
//!
//! This module provides an API client that can query a remote nxv server
//! instead of the local SQLite database. Enable by setting `NXV_API_URL`.

use crate::db::queries::{IndexStats, PackageVersion, VersionHistoryEntry};
use crate::error::{NxvError, Result};
use crate::search::{SearchOptions, SearchResult, SortOrder};
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::Deserialize;
use std::time::Duration;

/// Response wrapper matching the API format.
#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    data: T,
    meta: Option<PaginationMeta>,
}

#[derive(Debug, Deserialize)]
struct PaginationMeta {
    total: usize,
    limit: usize,
    #[allow(dead_code)]
    offset: usize,
    has_more: bool,
}

/// Version history entry from API (for deserialization).
///
/// Note: `is_insecure` uses `#[serde(default)]` to default to `false` when
/// connecting to older API versions that don't include this field. This ensures
/// backwards compatibility - older servers simply report all versions as secure.
#[derive(Debug, Deserialize)]
struct ApiVersionHistoryEntry {
    version: String,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
    /// Whether this version has known vulnerabilities. Defaults to `false`
    /// for backwards compatibility with older API versions.
    #[serde(default)]
    is_insecure: bool,
}

/// HTTP client for the nxv API.
pub struct ApiClient {
    base_url: String,
    client: Client,
}

/// Default timeout for API requests in seconds.
#[cfg(test)]
const DEFAULT_TIMEOUT_SECS: u64 = 30;

impl ApiClient {
    /// Creates a new ApiClient with a normalized base URL and a configured HTTP client.
    ///
    /// The `base_url` is normalized by removing a trailing slash if present. The internal
    /// blocking HTTP client timeout defaults to 30 seconds but can be overridden via
    /// the `NXV_API_TIMEOUT` environment variable (in seconds).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use nxv::client::ApiClient;
    /// let client = ApiClient::new("https://example.com").unwrap();
    /// // client is ready to make API requests
    /// ```
    ///
    /// Returns `Ok(ApiClient)` on success, or `Err(NxvError::Network)` if building the HTTP client fails.
    #[cfg(test)]
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let timeout_secs = std::env::var("NXV_API_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        Self::new_with_timeout(base_url, timeout_secs)
    }

    /// Creates a new ApiClient with a specified timeout.
    ///
    /// The `base_url` is normalized by removing a trailing slash if present.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use nxv::client::ApiClient;
    /// let client = ApiClient::new_with_timeout("https://example.com", 60).unwrap();
    /// // client is ready to make API requests with 60s timeout
    /// ```
    ///
    /// Returns `Ok(ApiClient)` on success, or `Err(NxvError::Network)` if building the HTTP client fails.
    pub fn new_with_timeout(base_url: impl Into<String>, timeout_secs: u64) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .map_err(NxvError::Network)?;

        Ok(Self { base_url, client })
    }

    /// Search for packages using the provided `SearchOptions`.
    ///
    /// The returned `SearchResult` contains matching `PackageVersion` entries and pagination
    /// metadata derived from the API response.
    ///
    /// # Returns
    ///
    /// `SearchResult` with matching package versions in `data`, the total number of matches in
    /// `total`, and whether more results are available in `has_more`.
    ///
    /// # Examples
    ///
    /// ```
    /// let client = ApiClient::new("https://nxv.example.com").unwrap();
    /// let opts = SearchOptions { query: "serde".into(), ..Default::default() };
    /// let result = client.search(&opts).unwrap();
    /// assert!(result.data.iter().any(|p| p.name.contains("serde")));
    /// ```
    pub fn search(&self, opts: &SearchOptions) -> Result<SearchResult> {
        let mut url = format!(
            "{}/api/v1/search?q={}",
            self.base_url,
            urlencoding::encode(&opts.query)
        );

        if let Some(ref version) = opts.version {
            url.push_str(&format!("&version={}", urlencoding::encode(version)));
        }
        if opts.exact {
            url.push_str("&exact=true");
        }
        if opts.desc {
            url.push_str("&desc=true");
        }
        if let Some(ref license) = opts.license {
            url.push_str(&format!("&license={}", urlencoding::encode(license)));
        }
        if let Some(ref platform) = opts.platform {
            url.push_str(&format!("&platform={}", urlencoding::encode(platform)));
        }

        let sort_str = match opts.sort {
            SortOrder::Relevance => "relevance",
            SortOrder::Date => "date",
            SortOrder::Version => "version",
            SortOrder::Name => "name",
        };
        url.push_str(&format!("&sort={}", sort_str));

        if opts.reverse {
            url.push_str("&reverse=true");
        }
        if opts.full {
            url.push_str("&full=true");
        }
        if opts.limit > 0 {
            url.push_str(&format!("&limit={}", opts.limit));
        }
        if opts.offset > 0 {
            url.push_str(&format!("&offset={}", opts.offset));
        }

        let response: ApiResponse<Vec<PackageVersion>> = self.get(&url)?;

        let (total, has_more, applied_limit) = match response.meta {
            Some(meta) => (meta.total, meta.has_more, Some(meta.limit)),
            None => (response.data.len(), false, None),
        };

        Ok(SearchResult {
            data: response.data,
            total,
            has_more,
            applied_limit,
        })
    }

    /// Retrieve packages that exactly match an attribute path.
    ///
    /// If the server returns 404 (package not found), this returns an empty vector.
    /// On other failures (network, service unavailable, unexpected HTTP status, or JSON deserialization),
    /// the error is propagated as an `NxvError`.
    ///
    /// # Parameters
    ///
    /// - `attr`: The exact attribute path of the package to fetch (e.g., "category/name").
    ///
    /// # Returns
    ///
    /// A `Vec<PackageVersion>` containing matching package versions; an empty vector if no package is found.
    ///
    /// # Examples
    ///
    /// ```
    /// let client = ApiClient::new("https://nxv.example.com").unwrap();
    /// let versions = client.get_package("some/package").unwrap();
    /// // `versions` will be empty if the package does not exist
    /// assert!(versions.is_empty() || !versions.is_empty());
    /// ```
    pub fn get_package(&self, attr: &str) -> Result<Vec<PackageVersion>> {
        let url = format!(
            "{}/api/v1/packages/{}",
            self.base_url,
            urlencoding::encode(attr)
        );

        match self.get::<ApiResponse<Vec<PackageVersion>>>(&url) {
            Ok(response) => Ok(response.data),
            Err(NxvError::PackageNotFound(_)) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// Search packages by name and optionally filter results to a specific version.
    ///
    /// The `package` argument is matched against package names; when `version` is
    /// provided only entries for that exact version are returned.
    ///
    /// The `limit` parameter controls pagination:
    /// - `None` defaults to 1000 results (suitable for most use cases)
    /// - `Some(0)` requests unlimited results (API default, may be slow for broad queries)
    /// - `Some(n)` limits to `n` results
    ///
    /// Note: Large result sets may be truncated by the server. If you need all matching
    /// packages, consider using `Some(0)` or paginating through results.
    ///
    /// # Examples
    ///
    /// ```
    /// let client = ApiClient::new("https://nxv.example.com").unwrap();
    /// // Default limit of 1000
    /// let results = client.search_by_name_version("serde", Some("1.0.0"), None).unwrap();
    /// // Explicit limit
    /// let limited = client.search_by_name_version("python", None, Some(50)).unwrap();
    /// ```
    pub fn search_by_name_version(
        &self,
        package: &str,
        version: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<PackageVersion>> {
        let mut url = format!(
            "{}/api/v1/search?q={}",
            self.base_url,
            urlencoding::encode(package)
        );

        if let Some(v) = version {
            url.push_str(&format!("&version={}", urlencoding::encode(v)));
        }

        // Apply limit: None defaults to 1000, Some(0) means unlimited (don't append), Some(n) uses n
        match limit {
            None => url.push_str("&limit=1000"),
            Some(0) => {} // No limit parameter = API default (unlimited)
            Some(n) => url.push_str(&format!("&limit={}", n)),
        }

        let response: ApiResponse<Vec<PackageVersion>> = self.get(&url)?;
        Ok(response.data)
    }

    /// Retrieves the first observed package version for the given package attribute and version.
    ///
    /// This method calls the `/api/v1/packages/{attr}/versions/{version}/first` endpoint.
    /// Not currently used by the CLI but provided for API completeness as a library method.
    ///
    /// # Parameters
    /// - `attr`: package attribute path (for example `"namespace/name"`).
    /// - `version`: version string to query (for example `"1.2.3"`).
    ///
    /// # Returns
    /// `Some(PackageVersion)` if a first occurrence exists for the specified package and version, `None` if the package/version is not found.
    ///
    /// # Examples
    ///
    /// ```
    /// let client = ApiClient::new("https://nxv.example.com").unwrap();
    /// let first = client.get_first_occurrence("pkg/name", "1.2.3").unwrap();
    /// if let Some(pv) = first {
    ///     assert_eq!(pv.version, "1.2.3");
    /// }
    /// ```
    #[allow(dead_code)]
    pub fn get_first_occurrence(
        &self,
        attr: &str,
        version: &str,
    ) -> Result<Option<PackageVersion>> {
        let url = format!(
            "{}/api/v1/packages/{}/versions/{}/first",
            self.base_url,
            urlencoding::encode(attr),
            urlencoding::encode(version)
        );

        match self.get::<ApiResponse<PackageVersion>>(&url) {
            Ok(response) => Ok(Some(response.data)),
            Err(NxvError::PackageNotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Fetches the last observed occurrence of a package version from the remote API.
    ///
    /// This method calls the `/api/v1/packages/{attr}/versions/{version}/last` endpoint.
    /// Not currently used by the CLI but provided for API completeness as a library method.
    ///
    /// Returns `Ok(Some(PackageVersion))` when the version was found, `Ok(None)` when the package or version is not present (404), and `Err(NxvError)` for other errors.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// let client = ApiClient::new("https://example.com").unwrap();
    /// let last = client.get_last_occurrence("pkg/name", "1.2.3").unwrap();
    /// if let Some(ver) = last {
    ///     println!("Last seen at: {}", ver.first_seen);
    /// }
    /// ```
    #[allow(dead_code)]
    pub fn get_last_occurrence(&self, attr: &str, version: &str) -> Result<Option<PackageVersion>> {
        let url = format!(
            "{}/api/v1/packages/{}/versions/{}/last",
            self.base_url,
            urlencoding::encode(attr),
            urlencoding::encode(version)
        );

        match self.get::<ApiResponse<PackageVersion>>(&url) {
            Ok(response) => Ok(Some(response.data)),
            Err(NxvError::PackageNotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Retrieves the version history for a package attribute.
    ///
    /// If the package does not exist, returns an empty vector.
    ///
    /// # Returns
    ///
    /// A vector of `VersionHistoryEntry` tuples in the form `(version, first_seen, last_seen, is_insecure)`.
    ///
    /// # Examples
    ///
    /// ```
    /// let client = ApiClient::new("https://nxv.example.com").unwrap();
    /// let history = client.get_version_history("org/package").unwrap();
    /// for (version, first_seen, last_seen, is_insecure) in history {
    ///     println!("{}: {} - {} (insecure: {})", version, first_seen, last_seen, is_insecure);
    /// }
    /// ```
    pub fn get_version_history(&self, attr: &str) -> Result<Vec<VersionHistoryEntry>> {
        let url = format!(
            "{}/api/v1/packages/{}/history",
            self.base_url,
            urlencoding::encode(attr)
        );

        match self.get::<ApiResponse<Vec<ApiVersionHistoryEntry>>>(&url) {
            Ok(response) => Ok(response
                .data
                .into_iter()
                .map(|e| (e.version, e.first_seen, e.last_seen, e.is_insecure))
                .collect()),
            Err(NxvError::PackageNotFound(_)) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// Fetches index statistics from the remote API.
    ///
    /// # Returns
    ///
    /// `IndexStats` containing the current index metrics returned by the server.
    ///
    /// # Examples
    ///
    /// ```
    /// let client = ApiClient::new("https://nxv.example.com").unwrap();
    /// let stats = client.get_stats().unwrap();
    /// // inspect some field on IndexStats, e.g. stats.total_packages (field names may vary)
    /// println!("{:?}", stats);
    /// ```
    pub fn get_stats(&self) -> Result<IndexStats> {
        let url = format!("{}/api/v1/stats", self.base_url);
        let response: ApiResponse<IndexStats> = self.get(&url)?;
        Ok(response.data)
    }

    /// Fetches the server health information, including the index commit when present.
    ///
    /// # Returns
    /// `HealthInfo` containing `status`, `version`, and an optional `index_commit`.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// let client = ApiClient::new("http://example.com").unwrap();
    /// let info = client.get_health().unwrap();
    /// println!("{}", info.status);
    /// ```
    pub fn get_health(&self) -> Result<HealthInfo> {
        let url = format!("{}/api/v1/health", self.base_url);
        self.get(&url)
    }

    /// Search packages by name with optional exact matching.
    ///
    /// When `exact` is `false`, performs a prefix search; when `exact` is `true`, requires an exact name match.
    /// The request forces a limit of 1000 results.
    ///
    /// # Parameters
    ///
    /// - `name`: Package name or prefix to search for.
    /// - `exact`: If `true`, only packages whose name exactly matches `name` are returned.
    ///
    /// # Returns
    ///
    /// `Vec<PackageVersion>` containing the matching package versions.
    ///
    /// # Examples
    ///
    /// ```
    /// let client = ApiClient::new("https://api.example.com").unwrap();
    /// let results = client.search_by_name("serde", false).unwrap();
    /// assert!(results.len() <= 1000);
    /// ```
    pub fn search_by_name(&self, name: &str, exact: bool) -> Result<Vec<PackageVersion>> {
        let mut url = format!(
            "{}/api/v1/search?q={}",
            self.base_url,
            urlencoding::encode(name)
        );

        if exact {
            url.push_str("&exact=true");
        }
        url.push_str("&limit=1000");

        let response: ApiResponse<Vec<PackageVersion>> = self.get(&url)?;
        Ok(response.data)
    }

    /// Send an HTTP GET request to `url` and deserialize the JSON response into `T`.
    ///
    /// If the response has a non-success HTTP status, an appropriate `NxvError` is returned.
    /// Network or JSON deserialization failures are returned as `NxvError::Network`.
    ///
    /// # Examples
    ///
    /// ```
    /// use serde_json::Value;
    ///
    /// // `client` is an instance of `ApiClient`
    /// // let client = ApiClient::new("https://api.example.com").unwrap();
    /// // let value: Value = client.get("https://api.example.com/api/v1/health").unwrap();
    /// ```
    fn get<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let response = self.client.get(url).send().map_err(NxvError::Network)?;

        let status = response.status();
        if !status.is_success() {
            return Err(self.map_status_error(status, url));
        }

        response.json().map_err(NxvError::Network)
    }

    /// Convert an HTTP status code into the corresponding `NxvError` variant.
    ///
    /// Maps `404 Not Found` to `NxvError::PackageNotFound`, `503 Service Unavailable`
    /// to `NxvError::NoIndex`, and all other status codes to `NxvError::ApiError` with
    /// the status code and URL included for debugging.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// let client = ApiClient::new("http://example.com").unwrap();
    /// let err = client.map_status_error(reqwest::StatusCode::NOT_FOUND, "/pkg");
    /// match err {
    ///     NxvError::PackageNotFound(_) => {},
    ///     _ => panic!("unexpected error variant"),
    /// }
    /// ```
    fn map_status_error(&self, status: StatusCode, url: &str) -> NxvError {
        match status {
            StatusCode::NOT_FOUND => NxvError::PackageNotFound("Resource not found".to_string()),
            StatusCode::SERVICE_UNAVAILABLE => NxvError::NoIndex,
            _ => NxvError::ApiError {
                status: status.as_u16(),
                url: url.to_string(),
            },
        }
    }
}

/// Health response from the API.
#[derive(Debug, Deserialize)]
pub struct HealthInfo {
    #[allow(dead_code)]
    pub status: String,
    #[allow(dead_code)]
    pub version: String,
    pub index_commit: Option<String>,
}

// URL encoding helper
mod urlencoding {
    /// Percent-encodes a string for use in URLs.
    ///
    /// Leaves unreserved characters (ASCII letters, digits, '-', '_', '.', '~') unchanged and
    /// percent-encodes every other character by UTF-8 byte, using uppercase hex digits.
    ///
    /// # Examples
    ///
    /// ```
    /// assert_eq!(urlencoding::encode("a b"), "a%20b");
    /// assert_eq!(urlencoding::encode("é"), "%C3%A9");
    /// assert_eq!(urlencoding::encode("~safe~"), "~safe~");
    /// ```
    pub fn encode(s: &str) -> String {
        let mut result = String::with_capacity(s.len() * 3);
        for c in s.chars() {
            match c {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => result.push(c),
                _ => {
                    for b in c.to_string().as_bytes() {
                        result.push_str(&format!("%{:02X}", b));
                    }
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod urlencoding_tests {
        use super::urlencoding;

        #[test]
        fn test_encode_unreserved_chars() {
            // Letters, digits, and unreserved chars should pass through unchanged
            assert_eq!(urlencoding::encode("abcXYZ"), "abcXYZ");
            assert_eq!(urlencoding::encode("0123456789"), "0123456789");
            assert_eq!(urlencoding::encode("-_.~"), "-_.~");
        }

        #[test]
        fn test_encode_spaces() {
            assert_eq!(urlencoding::encode("hello world"), "hello%20world");
            assert_eq!(urlencoding::encode("  "), "%20%20");
        }

        #[test]
        fn test_encode_special_chars() {
            assert_eq!(urlencoding::encode("foo/bar"), "foo%2Fbar");
            assert_eq!(urlencoding::encode("a=b&c=d"), "a%3Db%26c%3Dd");
            assert_eq!(urlencoding::encode("test?query"), "test%3Fquery");
        }

        #[test]
        fn test_encode_unicode() {
            // UTF-8 encoding of 'é' is C3 A9
            assert_eq!(urlencoding::encode("café"), "caf%C3%A9");
            // UTF-8 encoding of '你' is E4 BD A0
            assert_eq!(urlencoding::encode("你好"), "%E4%BD%A0%E5%A5%BD");
        }

        #[test]
        fn test_encode_empty_string() {
            assert_eq!(urlencoding::encode(""), "");
        }

        #[test]
        fn test_encode_realistic_package_names() {
            // Common package name patterns
            assert_eq!(urlencoding::encode("python3"), "python3");
            assert_eq!(urlencoding::encode("gcc-12"), "gcc-12");
            assert_eq!(urlencoding::encode("node_modules"), "node_modules");
            assert_eq!(urlencoding::encode("nixpkgs.python3"), "nixpkgs.python3");
        }
    }

    mod api_client_tests {
        use super::*;
        use serial_test::serial;

        #[test]
        fn test_new_trims_trailing_slash() {
            let client = ApiClient::new("https://example.com/").unwrap();
            assert!(!client.base_url.ends_with('/'));
        }

        #[test]
        fn test_new_no_trailing_slash() {
            let client = ApiClient::new("https://example.com").unwrap();
            assert_eq!(client.base_url, "https://example.com");
        }

        #[test]
        fn test_new_multiple_trailing_slashes() {
            // trim_end_matches removes ALL matching chars, so both slashes are removed
            let client = ApiClient::new("https://example.com//").unwrap();
            assert_eq!(client.base_url, "https://example.com");
        }

        #[test]
        fn test_map_status_error_not_found() {
            let client = ApiClient::new("https://example.com").unwrap();
            let err = client.map_status_error(StatusCode::NOT_FOUND, "/test");
            assert!(matches!(err, NxvError::PackageNotFound(_)));
        }

        #[test]
        fn test_map_status_error_service_unavailable() {
            let client = ApiClient::new("https://example.com").unwrap();
            let err = client.map_status_error(StatusCode::SERVICE_UNAVAILABLE, "/test");
            assert!(matches!(err, NxvError::NoIndex));
        }

        #[test]
        fn test_map_status_error_other() {
            let client = ApiClient::new("https://example.com").unwrap();
            let err = client.map_status_error(StatusCode::INTERNAL_SERVER_ERROR, "/test");
            match err {
                NxvError::ApiError { status, url } => {
                    assert_eq!(status, 500);
                    assert_eq!(url, "/test");
                }
                _ => panic!("Expected ApiError"),
            }
        }

        #[test]
        #[serial(env)]
        fn test_timeout_env_var_invalid_ignored() {
            // Invalid values should fall back to default
            // SAFETY: Test isolation via #[serial(env)] ensures no concurrent access
            unsafe { std::env::set_var("NXV_API_TIMEOUT", "not_a_number") };
            let client = ApiClient::new("https://example.com").unwrap();
            assert!(!client.base_url.is_empty()); // Client created successfully
            unsafe { std::env::remove_var("NXV_API_TIMEOUT") };
        }

        #[test]
        #[serial(env)]
        fn test_timeout_env_var_valid() {
            // SAFETY: Test isolation via #[serial(env)] ensures no concurrent access
            unsafe { std::env::set_var("NXV_API_TIMEOUT", "60") };
            let client = ApiClient::new("https://example.com").unwrap();
            assert!(!client.base_url.is_empty()); // Client created successfully
            unsafe { std::env::remove_var("NXV_API_TIMEOUT") };
        }
    }
}

#[cfg(test)]
mod proptests {
    use super::urlencoding;
    use proptest::prelude::*;

    proptest! {
        /// URL encoding should never panic on any input.
        #[test]
        fn encode_never_panics(s in "\\PC*") {
            let _ = urlencoding::encode(&s);
        }

        /// Encoded strings should only contain safe URL characters.
        #[test]
        fn encode_produces_safe_chars(s in "[\\x00-\\x7F]{0,50}") {
            let encoded = urlencoding::encode(&s);
            for c in encoded.chars() {
                prop_assert!(
                    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '%'),
                    "Unexpected character in encoded string: {:?}",
                    c
                );
            }
        }

        /// Encoded strings should be at least as long as input.
        #[test]
        fn encode_length_reasonable(s in ".{0,30}") {
            let encoded = urlencoding::encode(&s);
            prop_assert!(encoded.len() >= s.len());
        }

        /// Unreserved characters should pass through unchanged.
        #[test]
        fn unreserved_chars_unchanged(s in "[a-zA-Z0-9._~-]+") {
            let encoded = urlencoding::encode(&s);
            prop_assert_eq!(encoded, s);
        }
    }
}

#[cfg(test)]
mod mock_api_tests {
    use super::*;

    /// Test search method with successful response
    #[test]
    fn test_search_success() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/search?q=hello&sort=relevance&limit=50")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "data": [{
                        "id": 1,
                        "name": "hello",
                        "version": "2.10",
                        "version_source": "direct",
                        "first_commit_hash": "abc123",
                        "first_commit_date": "2020-01-01T00:00:00Z",
                        "last_commit_hash": "def456",
                        "last_commit_date": "2020-06-01T00:00:00Z",
                        "attribute_path": "hello",
                        "description": "A greeting program",
                        "license": "GPL-3.0",
                        "homepage": null,
                        "maintainers": null,
                        "platforms": null,
                        "source_path": null,
                        "known_vulnerabilities": null,
                        "store_paths": {}
                    }],
                    "meta": {
                        "total": 1,
                        "limit": 50,
                        "offset": 0,
                        "has_more": false
                    }
                }"#,
            )
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let opts = SearchOptions {
            query: "hello".to_string(),
            ..Default::default()
        };
        let result = client.search(&opts).unwrap();

        mock.assert();
        assert_eq!(result.data.len(), 1);
        assert_eq!(result.data[0].name, "hello");
        assert_eq!(result.total, 1);
        assert!(!result.has_more);
    }

    /// Test search with version filter
    #[test]
    fn test_search_with_version() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/search?q=hello&version=2.10&sort=relevance&limit=50")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data": [], "meta": {"total": 0, "limit": 50, "offset": 0, "has_more": false}}"#)
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let opts = SearchOptions {
            query: "hello".to_string(),
            version: Some("2.10".to_string()),
            ..Default::default()
        };
        let result = client.search(&opts).unwrap();

        mock.assert();
        assert!(result.data.is_empty());
    }

    /// Test search with all options
    #[test]
    fn test_search_with_all_options() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", mockito::Matcher::Regex(
                r"/api/v1/search\?q=test.*exact=true.*desc=true.*license=MIT.*platform=x86_64-linux.*sort=date.*reverse=true.*full=true.*limit=10.*offset=5".to_string()
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data": []}"#)
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let opts = SearchOptions {
            query: "test".to_string(),
            version: None,
            exact: true,
            desc: true,
            license: Some("MIT".to_string()),
            platform: Some("x86_64-linux".to_string()),
            sort: SortOrder::Date,
            reverse: true,
            full: true,
            limit: 10,
            offset: 5,
        };
        let result = client.search(&opts).unwrap();

        mock.assert();
        assert!(result.data.is_empty());
    }

    /// Test get_package success
    #[test]
    fn test_get_package_success() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/packages/hello")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "data": [{
                        "id": 1,
                        "name": "hello",
                        "version": "2.10",
                        "version_source": null,
                        "first_commit_hash": "abc123",
                        "first_commit_date": "2020-01-01T00:00:00Z",
                        "last_commit_hash": "def456",
                        "last_commit_date": "2020-06-01T00:00:00Z",
                        "attribute_path": "hello",
                        "description": null,
                        "license": null,
                        "homepage": null,
                        "maintainers": null,
                        "platforms": null,
                        "source_path": null,
                        "known_vulnerabilities": null,
                        "store_paths": {}
                    }]
                }"#,
            )
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client.get_package("hello").unwrap();

        mock.assert();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].attribute_path, "hello");
    }

    /// Test get_package not found returns empty vec
    #[test]
    fn test_get_package_not_found() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/packages/nonexistent")
            .with_status(404)
            .with_body(r#"{"error": "Not found"}"#)
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client.get_package("nonexistent").unwrap();

        mock.assert();
        assert!(result.is_empty());
    }

    /// Test get_package with URL encoding
    #[test]
    fn test_get_package_url_encoding() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/packages/path%2Fto%2Fpackage")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data": []}"#)
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client.get_package("path/to/package").unwrap();

        mock.assert();
        assert!(result.is_empty());
    }

    /// Test search_by_name_version with version
    #[test]
    fn test_search_by_name_version_with_version() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/search?q=hello&version=2.10&limit=1000")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data": []}"#)
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client
            .search_by_name_version("hello", Some("2.10"), None)
            .unwrap();

        mock.assert();
        assert!(result.is_empty());
    }

    /// Test search_by_name_version with custom limit
    #[test]
    fn test_search_by_name_version_custom_limit() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/search?q=hello&limit=50")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data": []}"#)
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client
            .search_by_name_version("hello", None, Some(50))
            .unwrap();

        mock.assert();
        assert!(result.is_empty());
    }

    /// Test search_by_name_version with unlimited (0)
    #[test]
    fn test_search_by_name_version_unlimited() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/search?q=hello")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data": []}"#)
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client
            .search_by_name_version("hello", None, Some(0))
            .unwrap();

        mock.assert();
        assert!(result.is_empty());
    }

    /// Test get_first_occurrence success
    #[test]
    fn test_get_first_occurrence_success() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/packages/hello/versions/2.10/first")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "data": {
                        "id": 1,
                        "name": "hello",
                        "version": "2.10",
                        "version_source": "direct",
                        "first_commit_hash": "abc123",
                        "first_commit_date": "2020-01-01T00:00:00Z",
                        "last_commit_hash": "abc123",
                        "last_commit_date": "2020-01-01T00:00:00Z",
                        "attribute_path": "hello",
                        "description": null,
                        "license": null,
                        "homepage": null,
                        "maintainers": null,
                        "platforms": null,
                        "source_path": null,
                        "known_vulnerabilities": null,
                        "store_paths": {}
                    }
                }"#,
            )
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client.get_first_occurrence("hello", "2.10").unwrap();

        mock.assert();
        assert!(result.is_some());
        let pkg = result.unwrap();
        assert_eq!(pkg.version, "2.10");
        assert_eq!(pkg.first_commit_hash, "abc123");
    }

    /// Test get_first_occurrence not found
    #[test]
    fn test_get_first_occurrence_not_found() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/packages/hello/versions/9.99/first")
            .with_status(404)
            .with_body(r#"{"error": "Not found"}"#)
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client.get_first_occurrence("hello", "9.99").unwrap();

        mock.assert();
        assert!(result.is_none());
    }

    /// Test get_last_occurrence success
    #[test]
    fn test_get_last_occurrence_success() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/packages/hello/versions/2.10/last")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "data": {
                        "id": 1,
                        "name": "hello",
                        "version": "2.10",
                        "version_source": null,
                        "first_commit_hash": "abc123",
                        "first_commit_date": "2020-01-01T00:00:00Z",
                        "last_commit_hash": "def456",
                        "last_commit_date": "2020-06-01T00:00:00Z",
                        "attribute_path": "hello",
                        "description": null,
                        "license": null,
                        "homepage": null,
                        "maintainers": null,
                        "platforms": null,
                        "source_path": null,
                        "known_vulnerabilities": null,
                        "store_paths": {}
                    }
                }"#,
            )
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client.get_last_occurrence("hello", "2.10").unwrap();

        mock.assert();
        assert!(result.is_some());
        let pkg = result.unwrap();
        assert_eq!(pkg.last_commit_hash, "def456");
    }

    /// Test get_version_history success
    #[test]
    fn test_get_version_history_success() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/packages/hello/history")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "data": [
                        {
                            "version": "2.12",
                            "first_seen": "2021-01-01T00:00:00Z",
                            "last_seen": "2021-06-01T00:00:00Z",
                            "is_insecure": false
                        },
                        {
                            "version": "2.10",
                            "first_seen": "2020-01-01T00:00:00Z",
                            "last_seen": "2020-12-01T00:00:00Z",
                            "is_insecure": true
                        }
                    ]
                }"#,
            )
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client.get_version_history("hello").unwrap();

        mock.assert();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "2.12");
        assert!(!result[0].3); // is_insecure = false
        assert_eq!(result[1].0, "2.10");
        assert!(result[1].3); // is_insecure = true
    }

    /// Test get_version_history not found returns empty vec
    #[test]
    fn test_get_version_history_not_found() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/packages/nonexistent/history")
            .with_status(404)
            .with_body(r#"{"error": "Not found"}"#)
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client.get_version_history("nonexistent").unwrap();

        mock.assert();
        assert!(result.is_empty());
    }

    /// Test get_version_history backwards compatibility (no is_insecure field)
    #[test]
    fn test_get_version_history_backwards_compat() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/packages/old-api/history")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "data": [
                        {
                            "version": "1.0.0",
                            "first_seen": "2020-01-01T00:00:00Z",
                            "last_seen": "2020-06-01T00:00:00Z"
                        }
                    ]
                }"#,
            )
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client.get_version_history("old-api").unwrap();

        mock.assert();
        assert_eq!(result.len(), 1);
        assert!(!result[0].3); // is_insecure defaults to false
    }

    /// Test get_stats success
    #[test]
    fn test_get_stats_success() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/stats")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "data": {
                        "total_ranges": 1000,
                        "unique_names": 500,
                        "unique_versions": 750,
                        "oldest_commit_date": "2017-01-01T00:00:00Z",
                        "newest_commit_date": "2024-01-01T00:00:00Z",
                        "last_indexed_commit": "abc123def456",
                        "last_indexed_date": "2024-01-01"
                    }
                }"#,
            )
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client.get_stats().unwrap();

        mock.assert();
        assert_eq!(result.total_ranges, 1000);
        assert_eq!(result.unique_names, 500);
        assert_eq!(result.unique_versions, 750);
    }

    /// Test get_health success
    #[test]
    fn test_get_health_success() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "status": "healthy",
                    "version": "0.1.0",
                    "index_commit": "abc123"
                }"#,
            )
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client.get_health().unwrap();

        mock.assert();
        assert_eq!(result.status, "healthy");
        assert_eq!(result.version, "0.1.0");
        assert_eq!(result.index_commit, Some("abc123".to_string()));
    }

    /// Test get_health without index_commit
    #[test]
    fn test_get_health_no_index_commit() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "status": "healthy",
                    "version": "0.1.0",
                    "index_commit": null
                }"#,
            )
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client.get_health().unwrap();

        mock.assert();
        assert!(result.index_commit.is_none());
    }

    /// Test search_by_name with exact match
    #[test]
    fn test_search_by_name_exact() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/search?q=hello&exact=true&limit=1000")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data": []}"#)
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client.search_by_name("hello", true).unwrap();

        mock.assert();
        assert!(result.is_empty());
    }

    /// Test search_by_name without exact match
    #[test]
    fn test_search_by_name_prefix() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/search?q=hel&limit=1000")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data": []}"#)
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client.search_by_name("hel", false).unwrap();

        mock.assert();
        assert!(result.is_empty());
    }

    /// Test API error handling for 500 Internal Server Error
    #[test]
    fn test_api_error_500() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/stats")
            .with_status(500)
            .with_body(r#"{"error": "Internal server error"}"#)
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client.get_stats();

        mock.assert();
        assert!(result.is_err());
        match result.unwrap_err() {
            NxvError::ApiError { status, .. } => assert_eq!(status, 500),
            e => panic!("Expected ApiError, got: {:?}", e),
        }
    }

    /// Test API error handling for 503 Service Unavailable
    #[test]
    fn test_api_error_503() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/stats")
            .with_status(503)
            .with_body(r#"{"error": "Service unavailable"}"#)
            .create();

        let client = ApiClient::new_with_timeout(server.url(), 5).unwrap();
        let result = client.get_stats();

        mock.assert();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), NxvError::NoIndex));
    }
}
