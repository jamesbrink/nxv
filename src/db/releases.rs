//! The `releases` table: a ledger of channel releases (snapshots) and their
//! ingestion state.
//!
//! Every channel release discovered in the releases.nixos.org listing gets a
//! row here. Ingestion drives off this ledger: `pending` and retryable
//! `failed` rows form the work list; a release becomes `ingested` only after
//! its observations are committed and its monitors pass. There is no
//! checkpoint commit hash to strand — a gap is just a `pending` row that the
//! next run picks up.

use super::Database;
use crate::error::Result;
use chrono::{DateTime, TimeZone, Utc};
use rusqlite::Row;
use serde::{Deserialize, Serialize};

/// Earliest date a `packages.json.br` exists anywhere in the release bucket
/// (nixpkgs-20.09pre218523.4a3f9aced7f, 2020-03-27).
///
/// Releases older than this can only be ingested by evaluating them with
/// `nix-env`, i.e. under `nxv index --backfill-evals`. That makes it the line
/// between "unsettled and awaiting retry" and "unsettled and out of reach of
/// every scheduled run", which both the run report and `nxv stats` distinguish.
pub const PACKAGES_JSON_FIRST_ARTIFACT: &str = "2020-03-27T00:00:00Z";

/// [`PACKAGES_JSON_FIRST_ARTIFACT`] parsed, for use in queries.
pub fn eval_era_before() -> DateTime<Utc> {
    PACKAGES_JSON_FIRST_ARTIFACT
        .parse()
        .expect("PACKAGES_JSON_FIRST_ARTIFACT is a valid RFC 3339 timestamp")
}

/// Maximum automatic retry attempts before a release is parked as `skipped`.
pub const MAX_ATTEMPTS: i64 = 5;

/// Base backoff between retries of a failed release (doubles per attempt).
pub const RETRY_BASE_SECS: i64 = 60 * 60; // 1 hour

/// Where a release's package data comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseSource {
    /// `packages.json.br` exists for this release (2020-03-27 onward).
    PackagesJson,
    /// Pre-packages.json era: evaluate `nixexprs.tar.xz` with nix-env.
    NixEnv,
    /// A `--head-eval` observation of master HEAD (not a channel release).
    HeadEval,
}

impl ReleaseSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReleaseSource::PackagesJson => "packages_json",
            ReleaseSource::NixEnv => "nix_env",
            ReleaseSource::HeadEval => "head_eval",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "nix_env" => ReleaseSource::NixEnv,
            "head_eval" => ReleaseSource::HeadEval,
            _ => ReleaseSource::PackagesJson,
        }
    }
}

/// Ingestion status of a release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseStatus {
    Pending,
    Ingested,
    Failed,
    Skipped,
}

impl ReleaseStatus {
    pub fn from_str(s: &str) -> Self {
        match s {
            "ingested" => ReleaseStatus::Ingested,
            "failed" => ReleaseStatus::Failed,
            "skipped" => ReleaseStatus::Skipped,
            _ => ReleaseStatus::Pending,
        }
    }
}

/// One row of the `releases` ledger.
// Some fields are carried for stats/reporting surfaces rather than read by
// the pipeline itself.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ReleaseRecord {
    pub id: i64,
    pub channel: String,
    pub release_name: String,
    pub commit_hash: String,
    pub commit_count: Option<i64>,
    pub release_date: DateTime<Utc>,
    pub source: ReleaseSource,
    pub status: ReleaseStatus,
    pub attempts: i64,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub attr_count: Option<i64>,
    pub error: Option<String>,
    pub ingested_at: Option<DateTime<Utc>>,
}

impl ReleaseRecord {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let ts = |secs: Option<i64>| secs.and_then(|s| Utc.timestamp_opt(s, 0).single());
        Ok(ReleaseRecord {
            id: row.get(0)?,
            channel: row.get(1)?,
            release_name: row.get(2)?,
            commit_hash: row.get(3)?,
            commit_count: row.get(4)?,
            release_date: Utc
                .timestamp_opt(row.get::<_, i64>(5)?, 0)
                .single()
                .unwrap_or_else(Utc::now),
            source: ReleaseSource::from_str(&row.get::<_, String>(6)?),
            status: ReleaseStatus::from_str(&row.get::<_, String>(7)?),
            attempts: row.get(8)?,
            last_attempt_at: ts(row.get(9)?),
            attr_count: row.get(10)?,
            error: row.get(11)?,
            ingested_at: ts(row.get(12)?),
        })
    }
}

const RELEASE_COLUMNS: &str = "id, channel, release_name, commit_hash, commit_count, \
     release_date, source, status, attempts, last_attempt_at, attr_count, error, ingested_at";

/// Per-channel ingestion summary used by `nxv stats` and the monitor.
#[cfg_attr(not(feature = "indexer"), allow(dead_code))]
#[derive(Debug, Clone, Default)]
pub struct ChannelCoverage {
    pub channel: String,
    pub ingested: i64,
    pub pending: i64,
    pub failed: i64,
    pub skipped: i64,
    /// Newest ingested release, if any.
    pub newest_ingested: Option<ReleaseRecord>,
}

impl Database {
    /// Record a newly discovered release as `pending`. Existing rows are left
    /// untouched (INSERT OR IGNORE) so re-listing never clobbers state.
    ///
    /// Returns `true` if the release was new.
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    pub fn insert_release_pending(
        &self,
        channel: &str,
        release_name: &str,
        commit_hash: &str,
        commit_count: Option<i64>,
        release_date: DateTime<Utc>,
        source: ReleaseSource,
    ) -> Result<bool> {
        let changes = self.conn.execute(
            r#"
            INSERT OR IGNORE INTO releases
                (channel, release_name, commit_hash, commit_count, release_date, source, status)
            VALUES (?, ?, ?, ?, ?, ?, 'pending')
            "#,
            rusqlite::params![
                channel,
                release_name,
                commit_hash,
                commit_count,
                release_date.timestamp(),
                source.as_str(),
            ],
        )?;
        Ok(changes > 0)
    }

    /// The work list for an indexing run: all `pending` releases plus `failed`
    /// releases whose exponential backoff has elapsed, oldest first.
    ///
    /// With `include_terminal`, `skipped` releases are resurrected too
    /// (`--retry-failed`).
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    pub fn release_worklist(&self, include_terminal: bool) -> Result<Vec<ReleaseRecord>> {
        let now = Utc::now().timestamp();
        let mut stmt = self.conn.prepare(&format!(
            r#"
            SELECT {RELEASE_COLUMNS} FROM releases
            WHERE status = 'pending'
               OR (status = 'failed'
                   AND attempts < ?1
                   -- attempts is bumped at attempt START, so a once-failed row
                   -- has attempts = 1 and should wait exactly the base delay:
                   -- shift by attempts - 1 (MAX-guarded; SQLite's << misbehaves
                   -- on negative shift counts).
                   AND (?2 - COALESCE(last_attempt_at, 0)) > (?3 << MIN(MAX(attempts - 1, 0), 16)))
               OR (?4 AND status IN ('failed', 'skipped'))
            ORDER BY release_date ASC
            "#
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![MAX_ATTEMPTS, now, RETRY_BASE_SECS, include_terminal],
            ReleaseRecord::from_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Bump a release's attempt counter at the start of an ingestion attempt.
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    pub fn mark_release_attempt(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE releases SET attempts = attempts + 1, last_attempt_at = ? WHERE id = ?",
            rusqlite::params![Utc::now().timestamp(), id],
        )?;
        Ok(())
    }

    /// Mark a release failed with a reason. After [`MAX_ATTEMPTS`] the status
    /// becomes terminal `skipped` (resurrectable via `--retry-failed`).
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    pub fn mark_release_failed(&self, id: i64, error: &str) -> Result<()> {
        self.conn.execute(
            r#"
            UPDATE releases
               SET status = CASE WHEN attempts >= ?1 THEN 'skipped' ELSE 'failed' END,
                   error = ?2
             WHERE id = ?3
            "#,
            rusqlite::params![MAX_ATTEMPTS, error, id],
        )?;
        Ok(())
    }

    /// Mark a release as terminally skipped without consuming retry attempts.
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    pub fn mark_release_skipped(&self, id: i64, reason: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE releases SET status = 'skipped', error = ? WHERE id = ?",
            rusqlite::params![reason, id],
        )?;
        Ok(())
    }

    /// Atomically write a flush group: a batch of observations plus the
    /// `ingested` status for the releases they came from. Either everything
    /// commits or nothing does — a release can never be `ingested` without
    /// its rows.
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    pub fn commit_flush_group(
        &mut self,
        observations: &[crate::db::queries::PackageVersion],
        ingested: &[(i64, i64)], // (release id, attr_count)
    ) -> Result<usize> {
        let tx = self.conn.transaction()?;
        // Any successful package write makes derived attrs/stats stale until
        // the finish step rebuilds them. Invalidate in the same transaction so
        // an interrupted run falls back to source tables instead of old caches.
        if !observations.is_empty() {
            Database::invalidate_derived_caches_tx(&tx)?;
        }
        let written = Database::upsert_observations_tx(&tx, observations)?;
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE releases SET status = 'ingested', attr_count = ?, error = NULL, \
                 ingested_at = ? WHERE id = ?",
            )?;
            let now = Utc::now().timestamp();
            for (id, attr_count) in ingested {
                stmt.execute(rusqlite::params![attr_count, now, id])?;
            }
        }
        tx.commit()?;
        Ok(written)
    }

    /// Newest ingested release, optionally constrained to one channel.
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    pub fn newest_ingested_release(&self, channel: Option<&str>) -> Result<Option<ReleaseRecord>> {
        let mut stmt = self.conn.prepare(&format!(
            r#"
            SELECT {RELEASE_COLUMNS} FROM releases
            WHERE status = 'ingested' AND (?1 IS NULL OR channel = ?1)
            ORDER BY release_date DESC LIMIT 1
            "#
        ))?;
        let mut rows = stmt.query_map(rusqlite::params![channel], ReleaseRecord::from_row)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// attr_counts of the `n` ingested releases of a channel+era closest
    /// BEFORE `before` — the monitor's rolling baseline.
    ///
    /// The date bound is what makes backfilling safe: a 2020 release must be
    /// compared against its chronological neighbors, not against the newest
    /// ingested data (the first full rebuild failed 2,634 perfectly good
    /// historical releases by holding them to a 2026-sized baseline).
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    pub fn recent_ingested_attr_counts(
        &self,
        channel: &str,
        source: ReleaseSource,
        n: usize,
        before: DateTime<Utc>,
    ) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT attr_count FROM releases
            WHERE status = 'ingested' AND channel = ? AND source = ? AND attr_count IS NOT NULL
              AND release_date < ?
            ORDER BY release_date DESC LIMIT ?
            "#,
        )?;
        let rows = stmt.query_map(
            rusqlite::params![channel, source.as_str(), before.timestamp(), n as i64],
            |row| row.get::<_, i64>(0),
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Per-channel status summary (for `nxv stats` and the run report).
    /// Returns an empty vec when the releases table doesn't exist (pre-v4 DB
    /// opened read-only).
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    pub fn channel_coverage(&self) -> Result<Vec<ChannelCoverage>> {
        let has_table: bool = self.conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='releases'",
            [],
            |row| row.get(0),
        )?;
        if !has_table {
            return Ok(Vec::new());
        }

        let mut stmt = self.conn.prepare(
            r#"
            SELECT channel,
                   SUM(status = 'ingested'),
                   SUM(status = 'pending'),
                   SUM(status = 'failed'),
                   SUM(status = 'skipped')
              FROM releases GROUP BY channel ORDER BY channel
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ChannelCoverage {
                channel: row.get(0)?,
                ingested: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                pending: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                failed: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                skipped: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                newest_ingested: None,
            })
        })?;

        let mut out = Vec::new();
        for row in rows {
            let mut cov = row?;
            cov.newest_ingested = self.newest_ingested_release(Some(&cov.channel))?;
            out.push(cov);
        }
        Ok(out)
    }

    /// Releases dated at or before `watermark_date` that are neither `ingested`
    /// nor `skipped`. Surfaced in the run report and `nxv stats`.
    ///
    /// Split by whether an ordinary run can still reach them. Rows from the
    /// pre-artifact nix-env era are only ever ingested under `--backfill-evals`,
    /// so a scheduled run will never retry them however many times it runs —
    /// reporting them as holes awaiting retry is simply false.
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    pub fn unsettled_releases_before(
        &self,
        watermark_date: DateTime<Utc>,
        eval_era_before: DateTime<Utc>,
    ) -> Result<UnsettledReleases> {
        self.conn
            .query_row(
                "SELECT COUNT(*), \
                        COALESCE(SUM(source = 'nix_env' AND release_date < ?2), 0) \
                 FROM releases \
                 WHERE release_date <= ?1 AND status NOT IN ('ingested', 'skipped')",
                rusqlite::params![watermark_date.timestamp(), eval_era_before.timestamp()],
                |row| {
                    Ok(UnsettledReleases {
                        total: row.get(0)?,
                        needs_eval: row.get(1)?,
                    })
                },
            )
            .map_err(Into::into)
    }
}

/// Breakdown of releases before the watermark that never settled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsettledReleases {
    /// All unsettled releases before the watermark.
    pub total: i64,
    /// Those only reachable with `--backfill-evals`, and so not retried by
    /// scheduled runs.
    pub needs_eval: i64,
}

impl UnsettledReleases {
    /// Unsettled releases an ordinary run will pick up again.
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    pub fn retryable(&self) -> i64 {
        self.total - self.needs_eval
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::queries::PackageVersion;
    use tempfile::tempdir;

    fn open_test_db() -> (tempfile::TempDir, Database) {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("test.db")).unwrap();
        (dir, db)
    }

    fn date(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    #[test]
    fn test_insert_release_pending_is_idempotent() {
        let (_dir, db) = open_test_db();

        let new = db
            .insert_release_pending(
                "nixpkgs-unstable",
                "nixpkgs-26.11pre1012902.8c3cede7ddc2",
                &"8c3cede7".repeat(5),
                Some(1_012_902),
                date(1_700_000_000),
                ReleaseSource::PackagesJson,
            )
            .unwrap();
        assert!(new);

        let again = db
            .insert_release_pending(
                "nixpkgs-unstable",
                "nixpkgs-26.11pre1012902.8c3cede7ddc2",
                &"8c3cede7".repeat(5),
                Some(1_012_902),
                date(1_700_000_000),
                ReleaseSource::PackagesJson,
            )
            .unwrap();
        assert!(!again, "re-listing must not clobber existing rows");
    }

    #[test]
    fn test_worklist_ordering_and_backoff() {
        let (_dir, db) = open_test_db();

        db.insert_release_pending(
            "nixpkgs-unstable",
            "release-b",
            "b",
            None,
            date(2_000),
            ReleaseSource::PackagesJson,
        )
        .unwrap();
        db.insert_release_pending(
            "nixpkgs-unstable",
            "release-a",
            "a",
            None,
            date(1_000),
            ReleaseSource::NixEnv,
        )
        .unwrap();

        let work = db.release_worklist(false).unwrap();
        assert_eq!(work.len(), 2);
        assert_eq!(work[0].release_name, "release-a", "oldest first");
        assert_eq!(work[0].source, ReleaseSource::NixEnv);

        // Fail release-a just now: backoff means it leaves the work list.
        db.mark_release_attempt(work[0].id).unwrap();
        db.mark_release_failed(work[0].id, "boom").unwrap();
        let work = db.release_worklist(false).unwrap();
        assert_eq!(work.len(), 1, "freshly failed release must back off");
        assert_eq!(work[0].release_name, "release-b");

        // But --retry-failed resurrects it.
        let work = db.release_worklist(true).unwrap();
        assert_eq!(work.len(), 2);
    }

    #[test]
    fn test_first_retry_waits_exactly_the_base_delay() {
        let (_dir, db) = open_test_db();
        db.insert_release_pending(
            "nixpkgs-unstable",
            "release-a",
            "a",
            None,
            date(1_000),
            ReleaseSource::PackagesJson,
        )
        .unwrap();
        let id = db.release_worklist(false).unwrap()[0].id;
        db.mark_release_attempt(id).unwrap();
        db.mark_release_failed(id, "boom").unwrap();

        let set_elapsed = |secs: i64| {
            db.conn
                .execute(
                    "UPDATE releases SET last_attempt_at = ? WHERE id = ?",
                    rusqlite::params![Utc::now().timestamp() - secs, id],
                )
                .unwrap();
        };

        // Just under the 1h base delay: still backing off.
        set_elapsed(RETRY_BASE_SECS - 100);
        assert!(
            db.release_worklist(false).unwrap().is_empty(),
            "first retry must not fire before the base delay"
        );

        // Just past it: eligible again (regression: the shift used `attempts`
        // instead of `attempts - 1`, doubling every delay).
        set_elapsed(RETRY_BASE_SECS + 100);
        assert_eq!(
            db.release_worklist(false).unwrap().len(),
            1,
            "first retry must fire after the base delay, not 2x it"
        );
    }

    #[test]
    fn test_failed_release_becomes_skipped_after_max_attempts() {
        let (_dir, db) = open_test_db();
        db.insert_release_pending(
            "nixpkgs-unstable",
            "release-a",
            "a",
            None,
            date(1_000),
            ReleaseSource::PackagesJson,
        )
        .unwrap();
        let id = db.release_worklist(false).unwrap()[0].id;

        for _ in 0..MAX_ATTEMPTS {
            db.mark_release_attempt(id).unwrap();
            db.mark_release_failed(id, "still broken").unwrap();
        }

        let status: String = db
            .conn
            .query_row("SELECT status FROM releases WHERE id = ?", [id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "skipped");
        assert!(db.release_worklist(false).unwrap().is_empty());
    }

    #[test]
    fn test_commit_flush_group_is_atomic_and_marks_ingested() {
        let (_dir, mut db) = open_test_db();
        db.insert_release_pending(
            "nixpkgs-unstable",
            "release-a",
            &"a".repeat(40),
            None,
            date(1_000),
            ReleaseSource::PackagesJson,
        )
        .unwrap();
        let id = db.release_worklist(false).unwrap()[0].id;

        let pkg = PackageVersion {
            id: 0,
            name: "hello".to_string(),
            version: "2.12".to_string(),
            first_commit_hash: "a".repeat(40),
            first_commit_date: date(1_000),
            last_commit_hash: "a".repeat(40),
            last_commit_date: date(1_000),
            attribute_path: "hello".to_string(),
            description: None,
            license: None,
            homepage: None,
            maintainers: None,
            platforms: None,
            source_path: None,
            known_vulnerabilities: None,
        };
        db.commit_flush_group(&[pkg], &[(id, 1)]).unwrap();

        let newest = db.newest_ingested_release(None).unwrap().unwrap();
        assert_eq!(newest.id, id);
        assert_eq!(newest.status, ReleaseStatus::Ingested);
        assert_eq!(newest.attr_count, Some(1));

        let coverage = db.channel_coverage().unwrap();
        assert_eq!(coverage.len(), 1);
        assert_eq!(coverage[0].ingested, 1);

        let unsettled = db.unsettled_releases_before(date(2_000), date(0)).unwrap();
        assert_eq!(unsettled, UnsettledReleases::default());
    }

    #[test]
    fn test_unsettled_releases_before() {
        let (_dir, db) = open_test_db();
        db.insert_release_pending(
            "nixpkgs-unstable",
            "release-a",
            "a",
            None,
            date(1_000),
            ReleaseSource::PackagesJson,
        )
        .unwrap();

        let unsettled = db
            .unsettled_releases_before(date(2_000), date(500))
            .unwrap();
        assert_eq!(unsettled.total, 1);
        assert_eq!(unsettled.needs_eval, 0);
        assert_eq!(unsettled.retryable(), 1);

        assert_eq!(
            db.unsettled_releases_before(date(500), date(500)).unwrap(),
            UnsettledReleases::default(),
            "the watermark bounds the count"
        );
    }

    /// Pre-artifact nix-env rows are counted apart: no scheduled run retries
    /// them, so reporting them as pending retry would be false.
    #[test]
    fn test_unsettled_releases_separates_eval_era() {
        let (_dir, db) = open_test_db();
        for (name, when, source) in [
            ("old-eval", date(100), ReleaseSource::NixEnv),
            ("probe-era", date(800), ReleaseSource::NixEnv),
            ("modern", date(1_000), ReleaseSource::PackagesJson),
        ] {
            db.insert_release_pending("nixpkgs-unstable", name, "a", None, when, source)
                .unwrap();
        }

        // Anything dated before the first artifact needs an evaluation; the
        // nix_env row after it is only a plan-time guess the probe can correct.
        let unsettled = db
            .unsettled_releases_before(date(2_000), date(500))
            .unwrap();
        assert_eq!(unsettled.total, 3);
        assert_eq!(unsettled.needs_eval, 1);
        assert_eq!(unsettled.retryable(), 2);
    }

    #[test]
    fn test_recent_ingested_attr_counts_filters_by_channel_and_source() {
        let (_dir, mut db) = open_test_db();
        for (i, (chan, source)) in [
            ("nixpkgs-unstable", ReleaseSource::PackagesJson),
            ("nixpkgs-unstable", ReleaseSource::NixEnv),
            ("nixos-unstable-small", ReleaseSource::PackagesJson),
        ]
        .iter()
        .enumerate()
        {
            db.insert_release_pending(
                chan,
                &format!("release-{i}"),
                &i.to_string(),
                None,
                date(1_000 + i as i64),
                *source,
            )
            .unwrap();
        }
        let ids: Vec<i64> = db
            .release_worklist(false)
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect();
        for (i, id) in ids.iter().enumerate() {
            db.commit_flush_group(&[], &[(*id, 100 * (i as i64 + 1))])
                .unwrap();
        }

        let counts = db
            .recent_ingested_attr_counts(
                "nixpkgs-unstable",
                ReleaseSource::PackagesJson,
                10,
                date(10_000),
            )
            .unwrap();
        assert_eq!(counts, vec![100]);

        // The date bound: nothing ingested before the very first release.
        let counts = db
            .recent_ingested_attr_counts(
                "nixpkgs-unstable",
                ReleaseSource::PackagesJson,
                10,
                date(1_000),
            )
            .unwrap();
        assert!(counts.is_empty());
    }
}
