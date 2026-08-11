//! Data-quality gates and the end-of-run coverage report.
//!
//! Gates run on the parsed snapshot BEFORE any row is written: a release
//! that fails its gate is marked `failed` and its data is dropped, so a
//! truncated download or upstream regression can never pollute
//! `package_versions`. This is the "catch missing packages EARLY" hard
//! requirement: the old indexer silently lost months of data (see
//! ANALYSIS.md); this one refuses the snapshot and pages the operator.

use crate::db::Database;
use crate::db::releases::{ReleaseRecord, ReleaseSource, UnsettledReleases, eval_era_before};
use crate::error::Result;
use crate::index::snapshot::SnapshotEntry;
use chrono::{DateTime, Datelike, Utc};
use serde::Serialize;
use std::collections::HashSet;

/// Rolling-baseline window: how many recent same-channel/same-era releases
/// feed the relative count floor.
const BASELINE_WINDOW: usize = 10;

/// Relative thresholds against the rolling baseline. Real nixpkgs history
/// contains legitimate step-function drops bigger than 10% (measured: -11.6%
/// in one advance in Jan 2021; python2 purge era in Apr 2021), and a hard
/// gate at 10% WEDGES: the baseline only updates from ingested releases, so
/// after a real level shift every subsequent release fails forever. So:
/// moderate drops warn (visible in the report/CI), only catastrophic drops
/// (the parser-bug class) hard-fail; the absolute year-ladder floor and
/// sentinels remain the unconditional hard gates.
const BASELINE_WARN_FRACTION: f64 = 0.90;
const BASELINE_HARD_FRACTION: f64 = 0.70;

/// Deaths in a single advance beyond this fraction of the total trigger an
/// advisory (mass renames are legitimate, e.g. python3xPackages flips).
const DEATHS_WARN_FRACTION: f64 = 0.05;

/// Births/deaths are only meaningful between snapshots that are actually
/// adjacent. Channel advances land hours apart; anything beyond this is a
/// coverage gap (a backfilled window meeting the present, a stalled channel),
/// where "attrs disappeared in one advance" would measure years of ordinary
/// churn rather than a single bad snapshot.
const MAX_ADVANCE_GAP_DAYS: i64 = 30;

/// `--strict` head-lag threshold: newest observation older than this fails CI.
pub const HEAD_LAG_STRICT_HOURS: i64 = 72;

/// Absolute package-count floor by era and year — a backstop under the
/// rolling baseline, calibrated against measured totals (61k 2020-03,
/// 69k 2021-06, 87k 2022-06, 101k 2023-06, 145k 2026-06; the nix-env era
/// is smaller and grows from ~10k in 2016).
pub fn absolute_floor(source: ReleaseSource, release_date: DateTime<Utc>) -> i64 {
    let year = release_date.year();
    match source {
        ReleaseSource::NixEnv => match year {
            ..=2016 => 8_000,
            2017 => 10_000,
            2018 => 12_000,
            2019 => 15_000,
            _ => 18_000,
        },
        ReleaseSource::PackagesJson | ReleaseSource::HeadEval => match year {
            ..=2020 => 50_000,
            2021 => 60_000,
            2022 => 75_000,
            2023 => 90_000,
            2024 => 100_000,
            2025 => 115_000,
            _ => 130_000,
        },
    }
}

/// A sentinel package that must exist in every snapshot within its window.
#[derive(Debug, Clone)]
pub struct Sentinel {
    /// Human-readable label for reports.
    pub label: String,
    /// Matcher over the snapshot's attribute paths.
    pub kind: SentinelKind,
    /// Only enforced for releases dated on/after this point.
    pub active_from: Option<DateTime<Utc>>,
    /// Only enforced for this source era (None = all eras).
    pub source: Option<ReleaseSource>,
}

#[derive(Debug, Clone)]
pub enum SentinelKind {
    /// The attribute path must be present verbatim.
    Exact(String),
    /// Any `python<N>Packages.requests` attr (N spanning 2/3.x era naming) —
    /// present across the entire bucket history, including 2016 nix-env
    /// output. The *unversioned* `python3Packages.*` set exists in NO
    /// packages.json era; never sentinel on it.
    PythonRequests,
}

impl Sentinel {
    fn matches(&self, attrs: &HashSet<&str>) -> bool {
        match &self.kind {
            SentinelKind::Exact(attr) => attrs.contains(attr.as_str()),
            SentinelKind::PythonRequests => attrs.iter().any(|a| {
                a.strip_prefix("python")
                    .and_then(|rest| rest.split_once("Packages.requests"))
                    .is_some_and(|(digits, rest)| {
                        !digits.is_empty()
                            && digits.bytes().all(|b| b.is_ascii_digit())
                            && rest.is_empty()
                    })
            }),
        }
    }

    fn applies(&self, release: &ReleaseRecord) -> bool {
        if let Some(from) = self.active_from
            && release.release_date < from
        {
            return false;
        }
        if let Some(source) = self.source
            && release.source != source
        {
            return false;
        }
        true
    }
}

/// Built-in sentinels per DESIGN.md §7, calibrated against real artifacts.
pub fn builtin_sentinels(extra_exact: &[String]) -> Vec<Sentinel> {
    let mut sentinels = vec![
        Sentinel {
            label: "firefox".to_string(),
            kind: SentinelKind::Exact("firefox".to_string()),
            active_from: None,
            source: None,
        },
        Sentinel {
            label: "thunderbird".to_string(),
            kind: SentinelKind::Exact("thunderbird".to_string()),
            active_from: None,
            source: None,
        },
        Sentinel {
            label: "python*Packages.requests".to_string(),
            kind: SentinelKind::PythonRequests,
            active_from: None,
            source: None,
        },
        Sentinel {
            // nh entered nixpkgs 2023-12; enforce from 2024 to dodge the
            // first few days of channel lag. This is the issue #23 package.
            label: "nh".to_string(),
            kind: SentinelKind::Exact("nh".to_string()),
            active_from: Some("2024-01-15T00:00:00Z".parse().expect("valid constant")),
            source: None,
        },
    ];
    for attr in extra_exact {
        sentinels.push(Sentinel {
            label: attr.clone(),
            kind: SentinelKind::Exact(attr.clone()),
            active_from: None,
            source: None,
        });
    }
    sentinels
}

/// Result of gating one release's parsed snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct GateResult {
    pub release_name: String,
    pub channel: String,
    pub attr_count: usize,
    /// Hard failures: the release must not be written.
    pub failures: Vec<String>,
    /// Advisories: logged and reported; fatal only under `--strict`
    /// (handled by the coordinator).
    pub warnings: Vec<String>,
    pub births: Option<usize>,
    pub deaths: Option<usize>,
}

impl GateResult {
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Gate one parsed snapshot. `prev_attrs` is the previous ingested release's
/// attribute set for the same channel within this run (None on the first
/// release of a run — births/deaths are then skipped; the count floors still
/// apply via the persisted rolling baseline).
pub fn evaluate_release_gate(
    db: &Database,
    release: &ReleaseRecord,
    entries: &[SnapshotEntry],
    prev_attrs: Option<(&HashSet<String>, ReleaseSource, DateTime<Utc>)>,
    sentinels: &[Sentinel],
) -> Result<GateResult> {
    let mut result = GateResult {
        release_name: release.release_name.clone(),
        channel: release.channel.clone(),
        attr_count: entries.len(),
        failures: Vec::new(),
        warnings: Vec::new(),
        births: None,
        deaths: None,
    };

    let attrs: HashSet<&str> = entries.iter().map(|e| e.attribute_path.as_str()).collect();

    // 1. Absolute year-ladder floor.
    let floor = absolute_floor(release.source, release.release_date);
    if (entries.len() as i64) < floor {
        result.failures.push(format!(
            "package count {} below the absolute floor {} for {} ({})",
            entries.len(),
            floor,
            release.release_date.format("%Y-%m"),
            release.source.as_str(),
        ));
    }

    // 2. Rolling baseline: same channel, same source era (so the 2020-03-27
    // era boundary never trips it), and only releases dated BEFORE this one
    // (so backfilling history is never held to a newer, larger baseline).
    // max() over the window keeps the baseline from ratcheting down if a
    // shrunken release slips through.
    let recent = db.recent_ingested_attr_counts(
        &release.channel,
        release.source,
        BASELINE_WINDOW,
        release.release_date,
    )?;
    if let Some(&baseline) = recent.iter().max() {
        let hard_floor = (baseline as f64 * BASELINE_HARD_FRACTION) as i64;
        let warn_floor = (baseline as f64 * BASELINE_WARN_FRACTION) as i64;
        let count = entries.len() as i64;
        if count < hard_floor {
            result.failures.push(format!(
                "package count {} is more than {:.0}% below the rolling baseline {} \
                 (max of last {} ingested {} releases)",
                count,
                (1.0 - BASELINE_HARD_FRACTION) * 100.0,
                baseline,
                recent.len(),
                release.channel,
            ));
        } else if count < warn_floor {
            result.warnings.push(format!(
                "package count {} is {:.1}% below the rolling baseline {} \
                 (legitimate mass-removals reach this range; verify if unexpected)",
                count,
                100.0 * (1.0 - count as f64 / baseline as f64),
                baseline,
            ));
        }
    }

    // 3. Sentinels.
    for sentinel in sentinels {
        if sentinel.applies(release) && !sentinel.matches(&attrs) {
            result.failures.push(format!(
                "sentinel package '{}' missing from snapshot",
                sentinel.label
            ));
        }
    }

    // 4. Births/deaths vs the previous snapshot in this run. Skipped across
    // era boundaries (the one-time +33k birth event at 2020-03-27 is
    // expected, not an anomaly) and across long gaps: a run that ingests a
    // backfilled 2020 window followed by today's release would otherwise
    // compare snapshots six years apart and report every retired attribute as
    // a mass death. Consecutive snapshots are hours apart, never weeks.
    if let Some((prev, prev_source, prev_date)) = prev_attrs
        && prev_source == release.source
        && (release.release_date - prev_date).num_days() <= MAX_ADVANCE_GAP_DAYS
    {
        let births = attrs.iter().filter(|a| !prev.contains(**a)).count();
        let deaths = prev.iter().filter(|a| !attrs.contains(a.as_str())).count();
        result.births = Some(births);
        result.deaths = Some(deaths);

        let warn_threshold = ((prev.len() as f64) * DEATHS_WARN_FRACTION) as usize;
        if deaths > warn_threshold.max(50) {
            let mut sample: Vec<&str> = prev
                .iter()
                .filter(|a| !attrs.contains(a.as_str()))
                .map(String::as_str)
                .take(8)
                .collect();
            sample.sort();
            result.warnings.push(format!(
                "{} attrs disappeared in one advance ({:.1}% of {}); sample: {}",
                deaths,
                100.0 * deaths as f64 / prev.len() as f64,
                prev.len(),
                sample.join(", ")
            ));
        }
    }

    Ok(result)
}

/// End-of-run coverage report (printed and optionally written as JSON).
#[derive(Debug, Default, Serialize)]
pub struct RunReport {
    pub planned: usize,
    pub ingested: usize,
    pub failed: usize,
    pub skipped: usize,
    /// Gate results for releases that failed or warned.
    pub anomalies: Vec<GateResult>,
    /// Per-channel newest ingested release.
    pub channels: Vec<ChannelReport>,
    /// Hours between now and the newest observation across channels.
    pub head_lag_hours: Option<i64>,
    /// Releases dated at or before the newest ingested observation that are
    /// neither ingested nor skipped, split by whether an ordinary run can still
    /// reach them. Publishing ingested progress is safe regardless (gate-failed
    /// snapshots never write rows), but a persistently nonzero `retryable`
    /// count means some window's versions stay missing until its release
    /// succeeds, while `needs_eval` will not move without `--backfill-evals`.
    pub unsettled_before_watermark: Option<UnsettledReleases>,
    /// Total distinct attribute paths in the index after the run.
    pub total_attrs: Option<i64>,
    /// Total (attr, version) rows after the run.
    pub total_rows: Option<i64>,
    pub strict: bool,
    /// Whether the run, as a whole, passes under the configured strictness.
    pub healthy: bool,
}

#[derive(Debug, Serialize)]
pub struct ChannelReport {
    pub channel: String,
    pub ingested: i64,
    pub pending: i64,
    pub failed: i64,
    pub skipped: i64,
    pub newest_release: Option<String>,
    pub newest_release_date: Option<DateTime<Utc>>,
    pub newest_attr_count: Option<i64>,
}

impl RunReport {
    /// Populate the DB-derived sections (coverage, totals, head lag).
    pub fn finalize(&mut self, db: &Database, strict: bool) -> Result<()> {
        self.strict = strict;

        for cov in db.channel_coverage()? {
            self.channels.push(ChannelReport {
                channel: cov.channel.clone(),
                ingested: cov.ingested,
                pending: cov.pending,
                failed: cov.failed,
                skipped: cov.skipped,
                newest_release: cov.newest_ingested.as_ref().map(|r| r.release_name.clone()),
                newest_release_date: cov.newest_ingested.as_ref().map(|r| r.release_date),
                newest_attr_count: cov.newest_ingested.as_ref().and_then(|r| r.attr_count),
            });
        }

        if let Some(newest) = db.newest_ingested_release(None)? {
            self.head_lag_hours = Some((Utc::now() - newest.release_date).num_hours());
            self.unsettled_before_watermark =
                Some(db.unsettled_releases_before(newest.release_date, eval_era_before())?);
        }

        let conn = db.connection();
        self.total_attrs = conn
            .query_row(
                "SELECT COUNT(DISTINCT attribute_path) FROM package_versions",
                [],
                |row| row.get(0),
            )
            .ok();
        self.total_rows = conn
            .query_row("SELECT COUNT(*) FROM package_versions", [], |row| {
                row.get(0)
            })
            .ok();

        let lag_ok = self
            .head_lag_hours
            .is_none_or(|lag| lag <= HEAD_LAG_STRICT_HOURS);
        let no_failures = self.failed == 0 && self.anomalies.iter().all(|a| a.failures.is_empty());
        let no_warnings_when_strict =
            !strict || self.anomalies.iter().all(|a| a.warnings.is_empty());
        // Lag affects health unconditionally: CI alerts on `healthy` even in
        // non-strict runs (publish-then-alert), and silent staleness is the
        // exact failure mode this rewrite exists to kill. --strict only
        // controls whether warnings are fatal and the process exit code.
        self.healthy = no_failures && no_warnings_when_strict && lag_ok;

        Ok(())
    }

    /// Human-readable summary to stderr.
    pub fn print(&self) {
        eprintln!();
        eprintln!("Index run report");
        eprintln!(
            "  releases: {} planned, {} ingested, {} failed, {} skipped",
            self.planned, self.ingested, self.failed, self.skipped
        );
        for ch in &self.channels {
            eprintln!(
                "  {}: {} ingested / {} pending / {} failed / {} skipped{}",
                ch.channel,
                ch.ingested,
                ch.pending,
                ch.failed,
                ch.skipped,
                match (&ch.newest_release, ch.newest_attr_count) {
                    (Some(name), Some(count)) => format!("; newest {name} ({count} attrs)"),
                    (Some(name), None) => format!("; newest {name}"),
                    _ => String::new(),
                }
            );
        }
        if let (Some(attrs), Some(rows)) = (self.total_attrs, self.total_rows) {
            eprintln!("  index: {attrs} distinct attrs, {rows} (attr, version) rows");
        }
        if let Some(unsettled) = self.unsettled_before_watermark
            && unsettled.total > 0
        {
            eprint!("  unsettled releases before watermark: {}", unsettled.total);
            if unsettled.needs_eval > 0 {
                eprint!(
                    " ({} pre-2020 nix-env era, unreachable without --backfill-evals",
                    unsettled.needs_eval
                );
                if unsettled.retryable() > 0 {
                    eprint!("; {} pending retry", unsettled.retryable());
                }
                eprint!(")");
            } else {
                eprint!(" (holes pending retry)");
            }
            eprintln!();
        }
        if let Some(lag) = self.head_lag_hours {
            eprintln!(
                "  head lag: {lag}h{}",
                if lag > HEAD_LAG_STRICT_HOURS {
                    " (EXCEEDS strict threshold)"
                } else {
                    ""
                }
            );
        }
        for anomaly in &self.anomalies {
            for failure in &anomaly.failures {
                eprintln!("  FAILED {} — {}", anomaly.release_name, failure);
            }
            for warning in &anomaly.warnings {
                eprintln!("  warn   {} — {}", anomaly.release_name, warning);
            }
        }
        eprintln!(
            "  status: {}",
            if self.healthy { "healthy" } else { "UNHEALTHY" }
        );
    }

    /// Write the report as JSON.
    pub fn write_json(&self, path: &std::path::Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(
            path,
            serde_json::to_vec_pretty(self).map_err(crate::error::NxvError::Json)?,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::releases::ReleaseStatus;
    use chrono::TimeZone;
    use tempfile::tempdir;

    fn release(channel: &str, source: ReleaseSource, date_secs: i64) -> ReleaseRecord {
        ReleaseRecord {
            id: 1,
            channel: channel.to_string(),
            release_name: "nixpkgs-test".to_string(),
            commit_hash: "a".repeat(40),
            commit_count: Some(1),
            release_date: Utc.timestamp_opt(date_secs, 0).unwrap(),
            source,
            status: ReleaseStatus::Pending,
            attempts: 0,
            last_attempt_at: None,
            attr_count: None,
            error: None,
            ingested_at: None,
        }
    }

    fn entry(attr: &str) -> SnapshotEntry {
        SnapshotEntry {
            attribute_path: attr.to_string(),
            name: attr.rsplit('.').next().unwrap().to_string(),
            version: "1.0".to_string(),
            description: None,
            license: None,
            homepage: None,
            maintainers: None,
            platforms: None,
            source_path: None,
            known_vulnerabilities: None,
        }
    }

    fn base_entries(n: usize) -> Vec<SnapshotEntry> {
        let mut entries: Vec<SnapshotEntry> = (0..n).map(|i| entry(&format!("pkg{i}"))).collect();
        entries.push(entry("firefox"));
        entries.push(entry("thunderbird"));
        entries.push(entry("python313Packages.requests"));
        entries.push(entry("nh"));
        entries
    }

    // 2026-era packages_json release date.
    const T_2026: i64 = 1_780_000_000;

    #[test]
    fn test_sentinel_python_requests_matching() {
        let s = Sentinel {
            label: "py".into(),
            kind: SentinelKind::PythonRequests,
            active_from: None,
            source: None,
        };
        let attrs: HashSet<&str> = ["python313Packages.requests"].into_iter().collect();
        assert!(s.matches(&attrs));
        let attrs: HashSet<&str> = ["python27Packages.requests"].into_iter().collect();
        assert!(s.matches(&attrs));
        // The unversioned set never exists; must not match other shapes.
        let attrs: HashSet<&str> = ["python3Packages.requests-not"].into_iter().collect();
        assert!(!s.matches(&attrs));
        let attrs: HashSet<&str> = ["pythonPackages.requests"].into_iter().collect();
        assert!(!s.matches(&attrs));
    }

    #[test]
    fn test_gate_passes_healthy_snapshot() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("t.db")).unwrap();
        let rel = release("nixpkgs-unstable", ReleaseSource::PackagesJson, T_2026);
        let entries = base_entries(140_000);

        let gate =
            evaluate_release_gate(&db, &rel, &entries, None, &builtin_sentinels(&[])).unwrap();
        assert!(gate.passed(), "failures: {:?}", gate.failures);
    }

    #[test]
    fn test_gate_fails_below_absolute_floor() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("t.db")).unwrap();
        let rel = release("nixpkgs-unstable", ReleaseSource::PackagesJson, T_2026);
        let entries = base_entries(5_000);

        let gate =
            evaluate_release_gate(&db, &rel, &entries, None, &builtin_sentinels(&[])).unwrap();
        assert!(!gate.passed());
        assert!(gate.failures[0].contains("absolute floor"));
    }

    #[test]
    fn test_gate_fails_on_missing_sentinel() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("t.db")).unwrap();
        let rel = release("nixpkgs-unstable", ReleaseSource::PackagesJson, T_2026);
        let mut entries = base_entries(140_000);
        entries.retain(|e| e.attribute_path != "nh");

        let gate =
            evaluate_release_gate(&db, &rel, &entries, None, &builtin_sentinels(&[])).unwrap();
        assert!(!gate.passed());
        assert!(gate.failures.iter().any(|f| f.contains("'nh'")));
    }

    #[test]
    fn test_nh_sentinel_window_not_enforced_in_2020() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("t.db")).unwrap();
        // 2020-07: packages_json era, before nh existed.
        let rel = release(
            "nixpkgs-unstable",
            ReleaseSource::PackagesJson,
            1_594_000_000,
        );
        let mut entries = base_entries(60_000);
        entries.retain(|e| e.attribute_path != "nh");

        let gate =
            evaluate_release_gate(&db, &rel, &entries, None, &builtin_sentinels(&[])).unwrap();
        assert!(gate.passed(), "failures: {:?}", gate.failures);
    }

    #[test]
    fn test_rolling_baseline_warns_on_moderate_drop_fails_on_catastrophic() {
        let dir = tempdir().unwrap();
        let mut db = Database::open(dir.path().join("t.db")).unwrap();

        // Seed an ingested release with a 200k count, dated BEFORE the
        // candidates (the baseline is date-bounded).
        db.insert_release_pending(
            "nixpkgs-unstable",
            "release-prev",
            &"b".repeat(40),
            None,
            Utc.timestamp_opt(T_2026 - 86_400, 0).unwrap(),
            ReleaseSource::PackagesJson,
        )
        .unwrap();
        let id = db.release_worklist(false).unwrap()[0].id;
        db.commit_flush_group(&[], &[(id, 200_000)]).unwrap();

        let rel = release("nixpkgs-unstable", ReleaseSource::PackagesJson, T_2026);

        // 15% below baseline (above the absolute floor): a plausible
        // real-world mass-removal — ingest with a warning. A hard gate here
        // wedges after legitimate level shifts (verified against the
        // Jan/Apr 2021 history).
        let entries = base_entries(170_000);
        let gate =
            evaluate_release_gate(&db, &rel, &entries, None, &builtin_sentinels(&[])).unwrap();
        assert!(
            gate.passed(),
            "moderate drops are advisory: {:?}",
            gate.failures
        );
        assert!(gate.warnings.iter().any(|w| w.contains("rolling baseline")));

        // 32.5% below baseline (still above the absolute floor): the
        // parser-bug class hard-fails.
        let entries = base_entries(135_000);
        let gate =
            evaluate_release_gate(&db, &rel, &entries, None, &builtin_sentinels(&[])).unwrap();
        assert!(!gate.passed());
        assert!(gate.failures.iter().any(|f| f.contains("rolling baseline")));
    }

    #[test]
    fn test_rolling_baseline_ignores_newer_releases_when_backfilling() {
        let dir = tempdir().unwrap();
        let mut db = Database::open(dir.path().join("t.db")).unwrap();

        // A modern release (145k attrs) is already ingested — the state the
        // first full rebuild started from.
        db.insert_release_pending(
            "nixpkgs-unstable",
            "release-modern",
            &"e".repeat(40),
            None,
            Utc.timestamp_opt(T_2026, 0).unwrap(),
            ReleaseSource::PackagesJson,
        )
        .unwrap();
        let id = db.release_worklist(false).unwrap()[0].id;
        db.commit_flush_group(&[], &[(id, 145_000)]).unwrap();

        // Backfilling a 2021 release with a legitimately smaller package
        // set must NOT be held to the 2026 baseline.
        let rel = release(
            "nixpkgs-unstable",
            ReleaseSource::PackagesJson,
            1_622_000_000, // 2021-05
        );
        let entries = base_entries(69_000);
        let gate =
            evaluate_release_gate(&db, &rel, &entries, None, &builtin_sentinels(&[])).unwrap();
        assert!(
            gate.passed(),
            "backfilled history must only be compared against earlier releases; failures: {:?}",
            gate.failures
        );
    }

    #[test]
    fn test_births_deaths_and_mass_death_warning() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("t.db")).unwrap();
        let rel = release("nixpkgs-unstable", ReleaseSource::PackagesJson, T_2026);

        let entries = base_entries(140_000);
        let mut prev: HashSet<String> = entries.iter().map(|e| e.attribute_path.clone()).collect();
        // Previous snapshot had 9k attrs that are now gone (>5%).
        for i in 0..9_000 {
            prev.insert(format!("vanished{i}"));
        }

        let gate = evaluate_release_gate(
            &db,
            &rel,
            &entries,
            Some((
                &prev,
                ReleaseSource::PackagesJson,
                Utc.timestamp_opt(T_2026 - 86_400, 0).unwrap(),
            )),
            &builtin_sentinels(&[]),
        )
        .unwrap();
        assert!(gate.passed(), "mass deaths are advisory, not fatal");
        assert_eq!(gate.deaths, Some(9_000));
        assert!(gate.warnings[0].contains("disappeared"));
    }

    #[test]
    fn test_births_deaths_skipped_across_era_boundary() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("t.db")).unwrap();
        // First packages_json release; previous snapshot was nix-env era.
        let rel = release(
            "nixpkgs-unstable",
            ReleaseSource::PackagesJson,
            1_585_267_200,
        );
        let entries = base_entries(61_000);
        let prev: HashSet<String> = (0..28_000).map(|i| format!("old{i}")).collect();

        let gate = evaluate_release_gate(
            &db,
            &rel,
            &entries,
            Some((
                &prev,
                ReleaseSource::NixEnv,
                Utc.timestamp_opt(1_585_267_200 - 86_400, 0).unwrap(),
            )),
            &builtin_sentinels(&[]),
        )
        .unwrap();
        assert!(gate.passed());
        assert_eq!(gate.births, None, "era boundary must skip births/deaths");
        assert!(gate.warnings.is_empty());
    }

    /// A run that ingests a backfilled 2020 window and then today's release
    /// compares snapshots six years apart. Every attribute retired since is
    /// not a mass death in "one advance", and under --strict the bogus warning
    /// would fail the run.
    #[test]
    fn test_births_deaths_skipped_across_long_gap() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("t.db")).unwrap();
        let rel = release("nixos-unstable-small", ReleaseSource::PackagesJson, T_2026);

        let entries = base_entries(140_000);
        // The 2020 snapshot: 61k attrs, almost none of them today's.
        let prev: HashSet<String> = (0..61_000).map(|i| format!("attr2020_{i}")).collect();
        let six_years_earlier = Utc.timestamp_opt(T_2026 - 6 * 365 * 86_400, 0).unwrap();

        let gate = evaluate_release_gate(
            &db,
            &rel,
            &entries,
            Some((&prev, ReleaseSource::PackagesJson, six_years_earlier)),
            &builtin_sentinels(&[]),
        )
        .unwrap();
        assert!(gate.passed());
        assert_eq!(
            gate.deaths, None,
            "snapshots years apart are not consecutive advances"
        );
        assert!(gate.warnings.is_empty());

        // A normal same-day advance is still measured.
        let gate = evaluate_release_gate(
            &db,
            &rel,
            &entries,
            Some((
                &prev,
                ReleaseSource::PackagesJson,
                Utc.timestamp_opt(T_2026 - 86_400, 0).unwrap(),
            )),
            &builtin_sentinels(&[]),
        )
        .unwrap();
        assert_eq!(gate.deaths, Some(61_000));
        assert!(!gate.warnings.is_empty());
    }
}
