//! Channel-release discovery from the releases.nixos.org S3 bucket.
//!
//! The bucket (`nix-releases`) is publicly listable. Each release lives in a
//! directory like `nixpkgs/nixpkgs-26.11pre1012902.8c3cede7ddc2/` containing
//! `git-revision` (the full 40-char commit hash), `packages.json.br`
//! (2020-03-27 onward) and `nixexprs.tar.xz`.
//!
//! Listing hazards handled here (all verified against the live bucket):
//! - `<Contents>` blocks may carry optional `<ChecksumAlgorithm>`/
//!   `<ChecksumType>` elements (post-Feb-2025) — parsing is event-based,
//!   never positional;
//! - ancient directory formats (`nixpkgs-0.5`, `1.0preNNN_shortrev`,
//!   `14.04`, ...) don't parse as releases and are skipped;
//! - release names are NOT lexicographically chronological
//!   (`pre1001117` < `pre880076`); ordering uses release dates / commit
//!   counts;
//! - the per-release HTML stub objects stopped being created in Feb 2025,
//!   so release dates come from the `git-revision` object's Last-Modified.

use crate::db::Database;
use crate::db::releases::ReleaseSource;
use crate::error::{NxvError, Result};
use chrono::{DateTime, Utc};
use std::io::Read;
use std::time::Duration;

/// Default S3 endpoint for release listings and artifact downloads.
pub const DEFAULT_BASE_URL: &str = "https://nix-releases.s3.amazonaws.com";

/// Verified earliest date a `packages.json.br` exists anywhere in the bucket
/// (nixpkgs-20.09pre218523.4a3f9aced7f, 2020-03-27). Releases dated well
/// after this that 404 on packages.json.br are real failures; releases
/// before mid-2020 that 404 get reclassified to the nix-env era.
pub const PACKAGES_JSON_SAFE_AFTER: &str = "2020-06-01T00:00:00Z";

// The date of that first artifact — `db::releases::PACKAGES_JSON_FIRST_ARTIFACT`,
// which lives there so `nxv stats` can report the same era boundary without the
// indexer feature. Between it and PACKAGES_JSON_SAFE_AFTER the bucket is mixed,
// so plan-time date classification guesses NixEnv and the fetch-time probe
// corrects it. Runs without --backfill-evals still admit that window: those
// releases cost one GET and no evaluation, and excluding them stranded ~200
// nixos-unstable-small snapshots that had artifacts all along.

/// A channel we ingest, mapped to its S3 prefix.
#[derive(Debug, Clone)]
pub struct ChannelSpec {
    /// Channel name as stored in the `releases.channel` column.
    pub name: String,
    /// S3 key prefix holding the release dirs (with trailing slash).
    pub s3_prefix: String,
}

/// The channels ingested by default: nixpkgs-unstable is the historical
/// spine (S3 history back to 2016), nixos-unstable-small is the currency
/// channel (typically hours behind master).
pub fn builtin_channels() -> Vec<ChannelSpec> {
    vec![
        ChannelSpec {
            name: "nixpkgs-unstable".to_string(),
            s3_prefix: "nixpkgs/".to_string(),
        },
        ChannelSpec {
            name: "nixos-unstable-small".to_string(),
            s3_prefix: "nixos/unstable-small/".to_string(),
        },
    ]
}

/// Resolve channel names (e.g. from `--channel`) to specs.
pub fn resolve_channels(names: &[String]) -> Result<Vec<ChannelSpec>> {
    let known = builtin_channels();
    let mut out = Vec::new();
    for name in names {
        match known.iter().find(|c| &c.name == name) {
            Some(spec) => out.push(spec.clone()),
            None => {
                // Allow arbitrary nixos channels by convention: the S3 layout
                // for nixos channels is nixos/<channel-suffix>/.
                if let Some(suffix) = name.strip_prefix("nixos-") {
                    out.push(ChannelSpec {
                        name: name.clone(),
                        s3_prefix: format!("nixos/{suffix}/"),
                    });
                } else {
                    return Err(NxvError::Config(format!(
                        "unknown channel '{name}' (known: nixpkgs-unstable, nixos-unstable-small, nixos-*)"
                    )));
                }
            }
        }
    }
    Ok(out)
}

/// Parsed release-directory name, e.g. `nixpkgs-26.11pre1012902.8c3cede7ddc2`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedReleaseName {
    /// Full directory name (without trailing slash).
    pub name: String,
    /// The `preNNNNNN` component — equals `git rev-list --count` of the
    /// release commit (verified), so it is a chronological ordering key.
    pub commit_count: i64,
    /// Abbreviated rev from the name (7-12 chars over the bucket's history).
    pub short_rev: String,
}

/// Parse a release dir name. Three verified shapes share the
/// `<channel>-YY.MM<sep><count>.<shortrev>` form:
///
/// - unstable: `nixpkgs-26.11pre1012902.8c3cede7ddc2` (sep = `pre`)
/// - stable:   `nixos-25.05.813814.ac62194c3917`      (sep = `.`)
/// - beta:     `nixos-25.05beta801537.2e1496bf8652`   (sep = `beta`)
///
/// Returns `None` for the ancient formats that aren't channel releases
/// (`nixpkgs-0.5`, `nixpkgs-1.0pre26905_1c8f786`, `nixpkgs-14.04`, ...).
pub fn parse_release_name(name: &str) -> Option<ParsedReleaseName> {
    let rest = name
        .strip_prefix("nixpkgs-")
        .or_else(|| name.strip_prefix("nixos-"))?;

    // rest = "26.11pre1012902.8c3cede7ddc2" | "25.05.813814.ac62194c3917" | ...
    let version_len = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    // Version must look like YY.MM; the all-digits-and-dots prefix of a
    // stable name ("25.05.813814.ac62194c3917") extends past it, so split
    // at the second dot when present.
    let mut dots = rest.char_indices().filter(|(_, c)| *c == '.');
    let _first_dot = dots.next()?;
    let version_end = match dots.next() {
        Some((second_dot, _)) if second_dot < version_len => second_dot,
        _ => version_len,
    };

    let after_version = &rest[version_end..];
    let counted = after_version
        .strip_prefix("pre")
        .or_else(|| after_version.strip_prefix("beta"))
        .or_else(|| after_version.strip_prefix('.'))?;

    let (count_str, short_rev) = counted.split_once('.')?;
    let commit_count: i64 = count_str.parse().ok()?;
    if short_rev.is_empty() || !short_rev.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }

    Some(ParsedReleaseName {
        name: name.to_string(),
        commit_count,
        short_rev: short_rev.to_string(),
    })
}

/// Percent-encode a query-string value (RFC 3986 unreserved characters pass
/// through). Needed for S3 continuation tokens, which carry `+`/`=`/`/`.
fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Minimal blocking HTTP client for the S3 bucket with retry/backoff.
/// Cheap to clone (reqwest clients share their connection pool).
#[derive(Clone)]
pub struct S3Client {
    client: reqwest::blocking::Client,
    base_url: String,
}

/// HTTP retry policy: attempts on 5xx/transport errors with exponential
/// backoff; 4xx is returned immediately (404 is a meaningful signal here).
const HTTP_RETRIES: u32 = 4;
const HTTP_BASE_DELAY_MS: u64 = 500;

impl S3Client {
    pub fn new(base_url: &str) -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(format!("nxv-indexer/{}", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(300))
            // S3 closes idle keep-alive connections after ~20s; reqwest's
            // default pool idle timeout (90s) happily reuses them, and in
            // the eval era a worker's connections sit idle for the full
            // 60-120s nix-env run — producing "response body error" failures
            // mid-download (observed at ~2.6% of eval-era releases). Keep
            // the pool fresher than S3's idle limit.
            .pool_idle_timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The underlying HTTP client, for requests outside the S3 bucket
    /// (e.g. the GitHub API in `--head-eval`).
    pub fn http(&self) -> &reqwest::blocking::Client {
        &self.client
    }

    /// Download and parse `<prefix><release>/packages.json.br`, streaming
    /// through brotli + JSON so the ~381 MB decompressed document is never
    /// materialized. A 404 surfaces as a `NetworkMessage` containing
    /// "HTTP 404" — the coordinator uses it to reclassify pre-boundary
    /// releases to the nix-env era.
    pub fn fetch_packages_json(
        &self,
        prefix: &str,
        release: &str,
    ) -> Result<Vec<crate::index::snapshot::SnapshotEntry>> {
        let url = format!("{}/{}{}/packages.json.br", self.base_url, prefix, release);
        let resp = self.get_with_retry(&url)?;
        if !resp.status().is_success() {
            return Err(NxvError::NetworkMessage(format!(
                "packages.json fetch failed: HTTP {} from {url}",
                resp.status()
            )));
        }
        crate::index::snapshot::parse_packages_json_br(resp)
    }

    /// Stream a URL to a file on disk (used for nixexprs/HEAD tarballs).
    ///
    /// The body copy is retried once: `get_with_retry` covers request-level
    /// failures, but a connection can still die mid-body (stale keep-alive,
    /// transient reset) after a 200.
    pub fn download_to_file(&self, url: &str, dest: &std::path::Path) -> Result<()> {
        let mut last_err: Option<NxvError> = None;
        for attempt in 0..2 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(HTTP_BASE_DELAY_MS));
            }
            let mut resp = self.get_with_retry(url)?;
            if !resp.status().is_success() {
                return Err(NxvError::NetworkMessage(format!(
                    "download failed: HTTP {} from {url}",
                    resp.status()
                )));
            }
            let mut file = std::fs::File::create(dest)?;
            match std::io::copy(&mut resp, &mut file) {
                Ok(_) => return Ok(()),
                Err(e) => last_err = Some(NxvError::Io(e)),
            }
        }
        Err(last_err
            .unwrap_or_else(|| NxvError::NetworkMessage(format!("download of {url} failed"))))
    }

    /// GET a URL with retry on transport errors and 5xx. Returns the final
    /// response (any status < 500) for the caller to interpret.
    fn get_with_retry(&self, url: &str) -> Result<reqwest::blocking::Response> {
        let mut last_err: Option<NxvError> = None;
        for attempt in 0..=HTTP_RETRIES {
            match self.client.get(url).send() {
                Ok(resp) if resp.status().is_server_error() => {
                    last_err = Some(NxvError::NetworkMessage(format!(
                        "HTTP {} from {url}",
                        resp.status()
                    )));
                }
                Ok(resp) => return Ok(resp),
                Err(e) => last_err = Some(NxvError::Network(e)),
            }
            if attempt < HTTP_RETRIES {
                std::thread::sleep(Duration::from_millis(
                    HTTP_BASE_DELAY_MS * 2u64.pow(attempt),
                ));
            }
        }
        Err(last_err
            .unwrap_or_else(|| NxvError::NetworkMessage(format!("request to {url} failed"))))
    }

    /// List release directory names under `prefix` via ListObjectsV2 with
    /// `delimiter=/` (paginated). Returns dir names without the prefix or
    /// trailing slash.
    pub fn list_release_dirs(&self, prefix: &str) -> Result<Vec<String>> {
        let mut dirs = Vec::new();
        let mut continuation: Option<String> = None;

        loop {
            let mut url = format!(
                "{}/?list-type=2&delimiter=%2F&prefix={}",
                self.base_url,
                encode_query_value(prefix)
            );
            if let Some(token) = &continuation {
                url.push_str("&continuation-token=");
                url.push_str(&encode_query_value(token));
            }

            let resp = self.get_with_retry(&url)?;
            if !resp.status().is_success() {
                return Err(NxvError::NetworkMessage(format!(
                    "S3 listing failed: HTTP {} from {url}",
                    resp.status()
                )));
            }
            let body = resp.text()?;
            let page = parse_list_page(&body)?;

            for full_prefix in page.common_prefixes {
                if let Some(dir) = full_prefix
                    .strip_prefix(prefix)
                    .map(|d| d.trim_end_matches('/'))
                    && !dir.is_empty()
                    && !dir.contains('/')
                {
                    dirs.push(dir.to_string());
                }
            }

            match (page.is_truncated, page.next_continuation_token) {
                (true, Some(token)) => continuation = Some(token),
                _ => break,
            }
        }

        Ok(dirs)
    }

    /// Fetch `<prefix><release>/git-revision`: the full commit hash plus the
    /// object's Last-Modified (the release date).
    pub fn fetch_git_revision(
        &self,
        prefix: &str,
        release: &str,
    ) -> Result<(String, DateTime<Utc>)> {
        let url = format!("{}/{}{}/git-revision", self.base_url, prefix, release);
        let resp = self.get_with_retry(&url)?;
        if !resp.status().is_success() {
            return Err(NxvError::NetworkMessage(format!(
                "git-revision fetch failed: HTTP {} from {url}",
                resp.status()
            )));
        }

        let last_modified = resp
            .headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| DateTime::parse_from_rfc2822(v).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .ok_or_else(|| {
                NxvError::NetworkMessage(format!("missing/invalid Last-Modified on {url}"))
            })?;

        let mut hash = String::new();
        resp.take(128).read_to_string(&mut hash)?;
        let hash = hash.trim().to_string();
        if hash.len() != 40 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(NxvError::NetworkMessage(format!(
                "invalid git-revision content at {url}: {hash:?}"
            )));
        }

        Ok((hash, last_modified))
    }
}

/// One page of a ListObjectsV2 response (the parts we use).
#[derive(Debug, Default)]
struct ListPage {
    common_prefixes: Vec<String>,
    is_truncated: bool,
    next_continuation_token: Option<String>,
}

/// Event-based parse of a ListObjectsV2 XML page. Tolerates unknown elements
/// anywhere (including the post-Feb-2025 `ChecksumAlgorithm`/`ChecksumType`
/// inside `<Contents>` that broke positional parsers).
fn parse_list_page(xml: &str) -> Result<ListPage> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut page = ListPage::default();
    let mut path: Vec<String> = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                path.push(String::from_utf8_lossy(e.name().as_ref()).to_string());
            }
            Ok(Event::End(_)) => {
                path.pop();
            }
            Ok(Event::Text(t)) => {
                let text = t.decode().map_err(|e| {
                    NxvError::NetworkMessage(format!("bad XML text in S3 listing: {e}"))
                })?;
                match path.last().map(String::as_str) {
                    Some("Prefix")
                        if path.len() >= 2 && path[path.len() - 2] == "CommonPrefixes" =>
                    {
                        page.common_prefixes.push(text.to_string());
                    }
                    Some("IsTruncated") => page.is_truncated = text == "true",
                    Some("NextContinuationToken") => {
                        page.next_continuation_token = Some(text.to_string());
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => {
                return Err(NxvError::NetworkMessage(format!(
                    "S3 listing XML parse error: {e}"
                )));
            }
        }
    }

    Ok(page)
}

/// Outcome of a planning pass.
#[derive(Debug, Default)]
pub struct PlanStats {
    pub dirs_seen: usize,
    pub skipped_unparseable: usize,
    pub new_releases: usize,
}

/// Discover releases for `channels` and record unknown ones as `pending`.
///
/// Only fetches `git-revision` for releases not already in the ledger, so
/// incremental runs cost one listing per channel plus one GET per new
/// release.
pub fn plan_releases(
    db: &Database,
    s3: &S3Client,
    channels: &[ChannelSpec],
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    progress: &dyn Fn(&str),
) -> Result<PlanStats> {
    let mut stats = PlanStats::default();
    let safe_after: DateTime<Utc> = PACKAGES_JSON_SAFE_AFTER
        .parse()
        .expect("valid constant timestamp");

    for channel in channels {
        progress(&format!(
            "listing {} ({}/{})...",
            channel.name,
            s3.base_url(),
            channel.s3_prefix
        ));
        let dirs = s3.list_release_dirs(&channel.s3_prefix)?;
        stats.dirs_seen += dirs.len();

        let known: std::collections::HashSet<String> = {
            let mut stmt = db
                .connection()
                .prepare("SELECT release_name FROM releases WHERE channel = ?")?;
            let rows = stmt.query_map([&channel.name], |row| row.get::<_, String>(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };

        let mut new_dirs: Vec<ParsedReleaseName> = Vec::new();
        for dir in dirs {
            match parse_release_name(&dir) {
                Some(parsed) => {
                    if !known.contains(&parsed.name) {
                        new_dirs.push(parsed);
                    }
                }
                None => stats.skipped_unparseable += 1,
            }
        }

        // Newest-first: commit_count is monotone with time (verified:
        // preNNNNNN == `git rev-list --count`), so when --since is set we can
        // stop fetching git-revision as soon as we descend past the window —
        // this keeps incremental planning to a handful of GETs instead of
        // one per historical release.
        new_dirs.sort_by_key(|p| std::cmp::Reverse(p.commit_count));

        if !new_dirs.is_empty() {
            progress(&format!(
                "{}: {} new release dirs to examine",
                channel.name,
                new_dirs.len()
            ));
        }

        for parsed in new_dirs {
            let (hash, release_date) = match s3.fetch_git_revision(&channel.s3_prefix, &parsed.name)
            {
                Ok(v) => v,
                Err(e) => {
                    // A release dir without a readable git-revision can't
                    // be ingested; skip it from planning (it will be
                    // retried next plan since it stays unknown).
                    progress(&format!("  skipping {}: {e}", parsed.name));
                    continue;
                }
            };

            // Sanity: the short rev in the name must prefix the full hash.
            if !hash.starts_with(&parsed.short_rev) {
                progress(&format!(
                    "  skipping {}: git-revision {} does not match name",
                    parsed.name,
                    &hash[..12]
                ));
                continue;
            }

            if let Some(since) = since
                && release_date < since
            {
                // Descending by commit count: every remaining dir is older.
                // (The handful of re-uploaded ancient dirs with bogus 2020
                // dates sit at the very bottom and are pruned with them.)
                break;
            }
            if let Some(until) = until
                && release_date > until
            {
                continue;
            }

            // Source guess: packages.json.br is verified gapless after the
            // safe-after date; earlier releases get probed at ingest time
            // (404 -> reclassified nix_env).
            let source = if release_date >= safe_after {
                ReleaseSource::PackagesJson
            } else {
                ReleaseSource::NixEnv
            };

            if db.insert_release_pending(
                &channel.name,
                &parsed.name,
                &hash,
                Some(parsed.commit_count),
                release_date,
                source,
            )? {
                stats.new_releases += 1;
            }
        }
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_release_name_modern() {
        let parsed = parse_release_name("nixpkgs-26.11pre1012902.8c3cede7ddc2").unwrap();
        assert_eq!(parsed.commit_count, 1_012_902);
        assert_eq!(parsed.short_rev, "8c3cede7ddc2");

        let parsed = parse_release_name("nixos-26.11pre1013756.f117b4b2ccc1").unwrap();
        assert_eq!(parsed.commit_count, 1_013_756);

        // 2017-era 7-char shortrev
        let parsed = parse_release_name("nixpkgs-17.03pre91913.cdec20a").unwrap();
        assert_eq!(parsed.commit_count, 91_913);
        assert_eq!(parsed.short_rev, "cdec20a");
    }

    #[test]
    fn test_parse_release_name_stable_and_beta() {
        // Stable channels: no `pre`, the count follows a plain dot.
        let parsed = parse_release_name("nixos-25.05.813814.ac62194c3917").unwrap();
        assert_eq!(parsed.commit_count, 813_814);
        assert_eq!(parsed.short_rev, "ac62194c3917");

        let parsed = parse_release_name("nixos-25.05beta801537.2e1496bf8652").unwrap();
        assert_eq!(parsed.commit_count, 801_537);
        assert_eq!(parsed.short_rev, "2e1496bf8652");
    }

    #[test]
    fn test_parse_release_name_rejects_ancient_formats() {
        assert!(parse_release_name("nixpkgs-0.5").is_none());
        assert!(parse_release_name("nixpkgs-14.04").is_none());
        // underscore separator instead of dot
        assert!(parse_release_name("nixpkgs-1.0pre26905_1c8f786").is_none());
        // no shortrev
        assert!(parse_release_name("nixpkgs-21.05pre287860").is_none());
        // not hex
        assert!(parse_release_name("nixpkgs-21.05pre287860.zzz").is_none());
        // unrelated dirs
        assert!(parse_release_name("17.09-darwin").is_none());
    }

    #[test]
    fn test_parse_list_page_with_post_2025_checksum_elements() {
        // Post-Feb-2025 listings embed ChecksumAlgorithm/ChecksumType inside
        // Contents; the parser must not be positional.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Name>nix-releases</Name>
  <Prefix>nixpkgs/</Prefix>
  <KeyCount>3</KeyCount>
  <IsTruncated>true</IsTruncated>
  <NextContinuationToken>token123==</NextContinuationToken>
  <Contents>
    <Key>nixpkgs/nixpkgs-26.11pre1012902.8c3cede7ddc2</Key>
    <LastModified>2026-06-08T16:32:11.000Z</LastModified>
    <ETag>&quot;abc&quot;</ETag>
    <ChecksumAlgorithm>CRC64NVME</ChecksumAlgorithm>
    <ChecksumType>FULL_OBJECT</ChecksumType>
    <Size>3000</Size>
    <StorageClass>STANDARD</StorageClass>
  </Contents>
  <CommonPrefixes>
    <Prefix>nixpkgs/nixpkgs-26.11pre1012902.8c3cede7ddc2/</Prefix>
  </CommonPrefixes>
  <CommonPrefixes>
    <Prefix>nixpkgs/nixpkgs-17.03pre91913.cdec20a/</Prefix>
  </CommonPrefixes>
  <CommonPrefixes>
    <Prefix>nixpkgs/17.09-darwin/</Prefix>
  </CommonPrefixes>
</ListBucketResult>"#;

        let page = parse_list_page(xml).unwrap();
        assert_eq!(page.common_prefixes.len(), 3);
        assert_eq!(
            page.common_prefixes[0],
            "nixpkgs/nixpkgs-26.11pre1012902.8c3cede7ddc2/"
        );
        assert!(page.is_truncated);
        assert_eq!(page.next_continuation_token.as_deref(), Some("token123=="));
    }

    #[test]
    fn test_parse_list_page_final_page() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <IsTruncated>false</IsTruncated>
</ListBucketResult>"#;
        let page = parse_list_page(xml).unwrap();
        assert!(!page.is_truncated);
        assert!(page.next_continuation_token.is_none());
        assert!(page.common_prefixes.is_empty());
    }

    #[test]
    fn test_resolve_channels() {
        let specs =
            resolve_channels(&["nixpkgs-unstable".to_string(), "nixos-unstable".to_string()])
                .unwrap();
        assert_eq!(specs[0].s3_prefix, "nixpkgs/");
        assert_eq!(specs[1].s3_prefix, "nixos/unstable/");

        assert!(resolve_channels(&["bogus".to_string()]).is_err());
    }
}
