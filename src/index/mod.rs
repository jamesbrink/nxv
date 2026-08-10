//! Index building from nixpkgs channel-release snapshots.
//!
//! The indexer ingests *observations*: "(attribute, version) was present in
//! channel release R at commit C". Sources, in order of preference:
//!
//! - `packages.json.br` per release from releases.nixos.org (2020-03-27 →
//!   today; no Nix evaluation at all),
//! - `nix-env -qaP --json --meta` over each release's `nixexprs.tar.xz`
//!   (2016-09-28 → 2020-03-27, behind `--backfill-evals`),
//! - an optional `--head-eval` pass over the GitHub tarball of master HEAD
//!   for channel-stuck periods.
//!
//! Pipeline (DESIGN.md §4): plan → parallel fetch/parse → in-order gate
//! (BEFORE any write) → aggregated widen-only upserts (atomic with the
//! release ledger) → finish (FTS/bloom rebuild, watermarks, report).
//!
//! See docs/indexer-rewrite/DESIGN.md for the full specification and
//! docs/indexer-rewrite/ANALYSIS.md for why the previous git-walking,
//! file-change-driven indexer was replaced.

pub mod eval;
pub mod monitor;
pub mod publisher;
pub mod releases;
pub mod snapshot;

use crate::bloom::PackageBloomFilter;
use crate::cli::{Cli, IndexArgs};
use crate::db::Database;
use crate::db::queries::PackageVersion;
use crate::db::releases::{ReleaseRecord, ReleaseSource};
use crate::error::{NxvError, Result};
use chrono::{DateTime, NaiveDate, Utc};
use monitor::RunReport;
use releases::S3Client;
use snapshot::SnapshotEntry;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

/// Flush the aggregator after this many gated releases (full/catch-up runs).
const FLUSH_GROUP_RELEASES: usize = 64;

/// ... or when it holds this many distinct (attr, version) pairs.
const FLUSH_GROUP_MAX_PAIRS: usize = 400_000;

/// Drop FTS triggers (and rebuild at the end) when ingesting at least this
/// many releases in one run.
const FTS_BULK_THRESHOLD: usize = 50;

/// `--head-eval` only fires when the newest channel observation is older
/// than this many hours.
const HEAD_EVAL_LAG_HOURS: i64 = 24;

/// Environment override for the release bucket URL (tests, mirrors).
const RELEASES_URL_ENV: &str = "NXV_RELEASES_URL";

/// Upstream channel snapshots known to be malformed. They are kept out of
/// ingestion permanently so data-quality gates can stay strict for every
/// newly discovered anomaly without making the scheduled publisher red forever.
const KNOWN_BAD_RELEASES: &[&str] = &[
    "nixos-24.11pre657868.4a8e77c70685",
    "nixpkgs-24.11pre657856.733453ac54a4",
];

fn known_bad_release_reason(release_name: &str) -> Option<&'static str> {
    if KNOWN_BAD_RELEASES.contains(&release_name) {
        Some("known malformed upstream snapshot: missing python*Packages.requests sentinel")
    } else {
        None
    }
}

/// Entry point for `nxv index`.
pub fn run_index(cli: &Cli, args: &IndexArgs) -> Result<()> {
    let mut db = Database::open(&cli.db_path)?;

    // Self-heal a dropped-triggers state: bulk runs drop the FTS sync
    // triggers and rebuild at the end — if a previous run died in between
    // (kill, OOM, power loss), description search would silently miss every
    // row written since. Detection is one sqlite_master query.
    if db.fts_triggers_missing()? {
        eprintln!(
            "warning: FTS triggers missing (a previous bulk run was interrupted); rebuilding FTS index..."
        );
        db.rebuild_fts()?;
    }

    let base_url =
        std::env::var(RELEASES_URL_ENV).unwrap_or_else(|_| releases::DEFAULT_BASE_URL.to_string());
    let s3 = S3Client::new(&base_url)?;

    let channel_names = args.channels.clone().unwrap_or_else(|| {
        releases::builtin_channels()
            .into_iter()
            .map(|c| c.name)
            .collect()
    });
    let channels = releases::resolve_channels(&channel_names)?;

    let since = parse_date_arg(args.since.as_deref(), "since")?;
    let until = parse_date_arg(args.until.as_deref(), "until")?;

    // Graceful Ctrl+C: finish the in-flight release group, then stop. An
    // interrupted run leaves pending rows that the next run picks up.
    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let flag = shutdown.clone();
        let _ = ctrlc::set_handler(move || {
            eprintln!("\nReceived Ctrl+C, finishing in-flight work...");
            flag.store(true, Ordering::SeqCst);
        });
    }

    let quiet = cli.quiet;
    let progress = |msg: &str| {
        if !quiet {
            eprintln!("{msg}");
        }
    };

    // ── Plan ────────────────────────────────────────────────────────────
    if args.full {
        let reset = db.connection().execute(
            "UPDATE releases SET status = 'pending', attempts = 0, error = NULL \
             WHERE status IN ('ingested', 'failed', 'skipped')",
            [],
        )?;
        if reset > 0 {
            progress(&format!("--full: re-queued {reset} known releases"));
        }
    }

    let plan = releases::plan_releases(&db, &s3, &channels, since, until, &progress)?;
    progress(&format!(
        "plan: {} release dirs seen, {} new, {} unparseable (skipped)",
        plan.dirs_seen, plan.new_releases, plan.skipped_unparseable
    ));

    // ── Work list ───────────────────────────────────────────────────────
    let channel_prefixes: HashMap<String, String> = channels
        .iter()
        .map(|c| (c.name.clone(), c.s3_prefix.clone()))
        .collect();

    let mut worklist: Vec<ReleaseRecord> = db
        .release_worklist(args.retry_failed)?
        .into_iter()
        .filter(|r| channel_prefixes.contains_key(&r.channel))
        .filter(|r| match (since, until) {
            (Some(s), _) if r.release_date < s => false,
            (_, Some(u)) if r.release_date > u => false,
            _ => true,
        })
        .filter(|r| args.backfill_evals || !needs_eval(r))
        .collect();
    if let Some(max) = args.max_releases {
        worklist.truncate(max);
    }

    let mut skipped_known_bad = 0usize;
    let mut filtered_worklist = Vec::with_capacity(worklist.len());
    for release in worklist {
        if let Some(reason) = known_bad_release_reason(&release.release_name) {
            progress(&format!("  skipping {}: {reason}", release.release_name));
            db.mark_release_skipped(release.id, reason)?;
            skipped_known_bad += 1;
        } else {
            filtered_worklist.push(release);
        }
    }
    let worklist = filtered_worklist;

    let mut report = RunReport {
        planned: worklist.len(),
        skipped: skipped_known_bad,
        ..Default::default()
    };

    let bulk = worklist.len() >= FTS_BULK_THRESHOLD;

    // Search-index recovery. A bulk run drops the covering index and rebuilds
    // it at the finish path; if a previous bulk run was interrupted,
    // `Database::open` left the index absent (the drop marker suppresses
    // init_schema's eager rebuild). Restore it now UNLESS this run is itself
    // bulk and will drop it again anyway — rebuilding a ~48s index only to
    // immediately drop it is pure waste. A non-bulk or no-work run must leave
    // the database search-ready.
    if !bulk && !db.search_index_present()? {
        progress("rebuilding search index (recovering interrupted bulk run)...");
        db.rebuild_search_index()?;
    }

    if worklist.is_empty() {
        progress("nothing to ingest: all known releases are settled");
    } else {
        progress(&format!("ingesting {} releases...", worklist.len()));
        let jobs = args.jobs.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get().min(4))
                .unwrap_or(2)
        });

        // Bulk catch-up: drop the FTS triggers and the covering search index so
        // the widen-only upserts avoid recurring write amplification; both are
        // rebuilt on the finish path below before the run returns.
        if bulk {
            db.drop_fts_triggers()?;
            db.drop_search_index()?;
        }

        let outcome = ingest_worklist(
            &mut db,
            &s3,
            &channel_prefixes,
            worklist,
            jobs,
            args.backfill_evals,
            &shutdown,
            &mut report,
            &progress,
        );

        // Rebuild before propagating any ingest error so the database is left
        // search/publish ready. A rebuild failure here propagates (leaving the
        // drop marker set for the next run to recover) rather than returning a
        // falsely-ready database.
        if bulk {
            progress("rebuilding FTS index...");
            db.rebuild_fts()?;
            progress("rebuilding search index...");
            db.rebuild_search_index()?;
        }
        outcome?;
    }

    // ── --head-eval: cover channel-stuck periods ────────────────────────
    // Failures here (network, GitHub API, eval) must not abort the finish
    // steps: the channel ingest above already succeeded and its bloom/
    // watermark/report work must still land.
    if args.head_eval
        && !shutdown.load(Ordering::SeqCst)
        && let Err(e) = run_head_eval(&mut db, &s3, args, &mut report, &progress)
    {
        progress(&format!("head-eval failed: {e} (continuing to finish)"));
        report.failed += 1;
    }

    // ── Finish ──────────────────────────────────────────────────────────
    progress("refreshing attribute cache...");
    db.refresh_package_attrs()?;

    progress("rebuilding bloom filter...");
    let bloom_path = crate::paths::get_bloom_path_for_db(&cli.db_path);
    save_bloom_filter(&db, &bloom_path)?;

    // Watermarks: load-bearing for manifest.latest_commit, `nxv sync`'s
    // UpToDate check, /health, /api/v1/stats and the frontend stats panel.
    if let Some(newest) = db.newest_ingested_release(None)? {
        db.set_meta("last_indexed_commit", &newest.commit_hash)?;
        db.set_meta("last_indexed_date", &Utc::now().to_rfc3339())?;
    }

    progress("refreshing stats cache...");
    db.refresh_stats_cache()?;

    report.finalize(&db, args.strict)?;
    if !quiet {
        report.print();
    }
    if let Some(path) = &args.report {
        report.write_json(path)?;
        progress(&format!("report written to {}", path.display()));
    }

    if shutdown.load(Ordering::SeqCst) {
        progress("interrupted: unfinished releases stay pending; run again to continue");
    }

    if args.strict && !report.healthy {
        return Err(NxvError::Config(
            "index run is unhealthy under --strict (see report above)".to_string(),
        ));
    }

    Ok(())
}

/// Parse a `--since`/`--until` argument (YYYY-MM-DD or RFC 3339).
fn parse_date_arg(value: Option<&str>, flag: &str) -> Result<Option<DateTime<Utc>>> {
    let Some(value) = value else { return Ok(None) };
    if let Ok(dt) = value.parse::<DateTime<Utc>>() {
        return Ok(Some(dt));
    }
    if let Ok(date) = value.parse::<NaiveDate>()
        && let Some(dt) = date.and_hms_opt(0, 0, 0)
    {
        return Ok(Some(dt.and_utc()));
    }
    Err(NxvError::Config(format!(
        "invalid --{flag} value {value:?}: expected YYYY-MM-DD or RFC 3339"
    )))
}

/// What a fetch worker hands back to the coordinator: the parsed entries
/// plus the mechanism that actually produced them (a release guessed as
/// nix-env era may turn out to have packages.json — see [`fetch_release`]).
type FetchResult = (usize, Result<(Vec<SnapshotEntry>, ReleaseSource)>);

/// Multi-release aggregator: merges K consecutive gated snapshots so row
/// writes are O(distinct pairs), not O(alive attrs × releases) — measured
/// churn is ~63 new pairs per advance vs ~144k alive rows.
#[derive(Default)]
struct Aggregator {
    pairs: HashMap<(String, String), PackageVersion>,
    releases: Vec<(i64, i64)>,
}

impl Aggregator {
    fn add_release(&mut self, release: &ReleaseRecord, entries: Vec<SnapshotEntry>) {
        let attr_count = entries.len() as i64;
        for entry in entries {
            let key = (entry.attribute_path.clone(), entry.version.clone());
            match self.pairs.get_mut(&key) {
                Some(existing) => {
                    // Widen bounds; metadata follows the newest observation.
                    if release.release_date < existing.first_commit_date {
                        existing.first_commit_hash = release.commit_hash.clone();
                        existing.first_commit_date = release.release_date;
                    }
                    if release.release_date >= existing.last_commit_date {
                        existing.last_commit_hash = release.commit_hash.clone();
                        existing.last_commit_date = release.release_date;
                        existing.description = entry.description;
                        existing.license = entry.license;
                        existing.homepage = entry.homepage;
                        existing.maintainers = entry.maintainers;
                        existing.platforms = entry.platforms;
                        if entry.source_path.is_some() {
                            existing.source_path = entry.source_path;
                        }
                        existing.known_vulnerabilities = entry.known_vulnerabilities;
                        existing.name = entry.name;
                    }
                }
                None => {
                    self.pairs.insert(
                        key,
                        PackageVersion {
                            id: 0,
                            name: entry.name,
                            version: entry.version,
                            first_commit_hash: release.commit_hash.clone(),
                            first_commit_date: release.release_date,
                            last_commit_hash: release.commit_hash.clone(),
                            last_commit_date: release.release_date,
                            attribute_path: entry.attribute_path,
                            description: entry.description,
                            license: entry.license,
                            homepage: entry.homepage,
                            maintainers: entry.maintainers,
                            platforms: entry.platforms,
                            source_path: entry.source_path,
                            known_vulnerabilities: entry.known_vulnerabilities,
                        },
                    );
                }
            }
        }
        self.releases.push((release.id, attr_count));
    }

    fn should_flush(&self) -> bool {
        self.releases.len() >= FLUSH_GROUP_RELEASES || self.pairs.len() >= FLUSH_GROUP_MAX_PAIRS
    }

    fn is_empty(&self) -> bool {
        self.releases.is_empty()
    }

    /// Write everything in one transaction: rows + `ingested` marks commit
    /// atomically (a release can never be `ingested` without its rows).
    fn flush(&mut self, db: &mut Database) -> Result<()> {
        if self.is_empty() {
            return Ok(());
        }
        let rows: Vec<PackageVersion> = std::mem::take(&mut self.pairs).into_values().collect();
        let marks = std::mem::take(&mut self.releases);
        db.commit_flush_group(&rows, &marks)?;
        Ok(())
    }
}

/// Whether a planned release can only be ingested by evaluating it with `nix`.
///
/// The plan-time source is a guess from the release date, and the fetch-time
/// probe is what settles it (DESIGN §2: probe, never date-classify) — but the
/// probe lives downstream of the work-list filter, so anything excluded here is
/// never probed at all. A `NixEnv` guess dated on or after the first known
/// `packages.json.br` is therefore admitted: worst case it 404s and is recorded
/// as needing `--backfill-evals`, best case it ingests with no evaluation.
fn needs_eval(release: &ReleaseRecord) -> bool {
    if release.source != ReleaseSource::NixEnv {
        return false;
    }
    release.release_date < crate::db::releases::eval_era_before()
}

/// Fetch + parse one release (worker side; no DB access).
///
/// The plan-time source is a guess from the release date; the truth is
/// per-release (DESIGN §2: probe, never date-classify). So nix-env-era
/// releases first probe packages.json.br — the releases between the first
/// artifact (2020-03-27) and the safe-after line get the zero-eval path.
///
/// `allow_eval` mirrors `--backfill-evals`. Without it a probe miss is an
/// error rather than a silent fallback to `nix-env`: the scheduled publish runs
/// have no `nix` and must not start evaluating a decade of history because one
/// artifact was missing.
fn fetch_release(
    s3: &S3Client,
    prefix: &str,
    release: &ReleaseRecord,
    allow_eval: bool,
) -> Result<(Vec<SnapshotEntry>, ReleaseSource)> {
    match release.source {
        ReleaseSource::PackagesJson => s3
            .fetch_packages_json(prefix, &release.release_name)
            .map(|entries| (entries, ReleaseSource::PackagesJson)),
        ReleaseSource::NixEnv => match s3.fetch_packages_json(prefix, &release.release_name) {
            Ok(entries) => Ok((entries, ReleaseSource::PackagesJson)),
            Err(NxvError::NetworkMessage(msg)) if msg.contains("HTTP 404") => {
                if !allow_eval {
                    return Err(NxvError::Config(format!(
                        "{} has no packages.json.br; re-run with --backfill-evals to evaluate it",
                        release.release_name
                    )));
                }
                eval::ingest_nix_env_release(s3, prefix, &release.release_name)
                    .map(|entries| (entries, ReleaseSource::NixEnv))
            }
            Err(e) => Err(e),
        },
        ReleaseSource::HeadEval => Err(NxvError::Config(
            "head_eval releases are ingested by the head-eval pass, not the worklist".to_string(),
        )),
    }
}

/// The parallel ingest pipeline over a planned work list.
#[allow(clippy::too_many_arguments)]
fn ingest_worklist(
    db: &mut Database,
    s3: &S3Client,
    channel_prefixes: &HashMap<String, String>,
    worklist: Vec<ReleaseRecord>,
    jobs: usize,
    allow_eval: bool,
    shutdown: &Arc<AtomicBool>,
    report: &mut RunReport,
    progress: &dyn Fn(&str),
) -> Result<()> {
    let total = worklist.len();
    let sentinels = monitor::builtin_sentinels(&[]);

    // Shared queue of work items; workers pull, fetch+parse, and send
    // (seq, result) back. Memory is bounded by THREE mechanisms together:
    // the sync_channel capacity (results awaiting the coordinator), the
    // reorder window below (workers refuse to run ahead of the coordinator,
    // so the seq-reorder buffer can't grow past WINDOW while one slow fetch
    // stalls next_seq), and per-release maps being dropped after write.
    let queue: Arc<Mutex<VecDeque<(usize, ReleaseRecord)>>> =
        Arc::new(Mutex::new(worklist.iter().cloned().enumerate().collect()));
    let (tx, rx) = mpsc::sync_channel::<FetchResult>(jobs.max(1));

    // Published by the coordinator after each processed seq; workers park
    // instead of claiming work more than WINDOW releases ahead of it.
    let coordinator_seq = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let window = (2 * jobs.max(1)).max(4);

    let mut handles = Vec::new();
    for _ in 0..jobs.max(1) {
        let queue = Arc::clone(&queue);
        let tx = tx.clone();
        let s3 = s3.clone();
        let prefixes = channel_prefixes.clone();
        let shutdown = Arc::clone(shutdown);
        let coordinator_seq = Arc::clone(&coordinator_seq);
        handles.push(std::thread::spawn(move || {
            loop {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                let claimed = {
                    let mut q = queue.lock().expect("queue lock");
                    match q.front() {
                        None => break, // queue drained
                        Some((seq, _))
                            if *seq > coordinator_seq.load(Ordering::Acquire) + window =>
                        {
                            None // too far ahead — park
                        }
                        Some(_) => q.pop_front(),
                    }
                };
                let Some((seq, release)) = claimed else {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    continue;
                };
                let prefix = prefixes.get(&release.channel).cloned().unwrap_or_default();
                let result = fetch_release(&s3, &prefix, &release, allow_eval);
                if tx.send((seq, result)).is_err() {
                    break;
                }
            }
        }));
    }
    drop(tx);

    // Coordinator: re-order results by seq so gating sees releases in
    // chronological order (births/deaths need consecutive snapshots).
    let mut buffer: BTreeMap<
        usize,
        std::result::Result<(Vec<SnapshotEntry>, ReleaseSource), NxvError>,
    > = BTreeMap::new();
    let mut next_seq = 0usize;
    let mut prev_attrs: HashMap<String, (HashSet<String>, ReleaseSource)> = HashMap::new();
    let mut aggregator = Aggregator::default();
    let mut hard_error: Option<NxvError> = None;

    'outer: for incoming in rx.iter() {
        buffer.insert(incoming.0, incoming.1);

        while let Some(result) = buffer.remove(&next_seq) {
            let mut release = worklist[next_seq].clone();
            next_seq += 1;
            coordinator_seq.store(next_seq, Ordering::Release);

            db.mark_release_attempt(release.id)?;

            match result {
                Ok((entries, actual_source)) => {
                    // A nix-env-era guess that turned out to have
                    // packages.json: record the real mechanism so the
                    // monitor's per-era baselines stay clean.
                    if release.source != actual_source {
                        db.connection().execute(
                            "UPDATE releases SET source = ? WHERE id = ?",
                            rusqlite::params![actual_source.as_str(), release.id],
                        )?;
                        release.source = actual_source;
                    }
                    let gate = monitor::evaluate_release_gate(
                        db,
                        &release,
                        &entries,
                        prev_attrs
                            .get(&release.channel)
                            .map(|(attrs, source)| (attrs, *source)),
                        &sentinels,
                    )?;

                    if !gate.passed() {
                        for failure in &gate.failures {
                            progress(&format!(
                                "  GATE FAILED {}: {failure}",
                                release.release_name
                            ));
                        }
                        db.mark_release_failed(release.id, &gate.failures.join("; "))?;
                        report.failed += 1;
                        report.anomalies.push(gate);
                        continue;
                    }
                    if !gate.warnings.is_empty() {
                        for warning in &gate.warnings {
                            progress(&format!("  warn {}: {warning}", release.release_name));
                        }
                        report.anomalies.push(gate.clone());
                    }

                    prev_attrs.insert(
                        release.channel.clone(),
                        (
                            entries.iter().map(|e| e.attribute_path.clone()).collect(),
                            release.source,
                        ),
                    );

                    aggregator.add_release(&release, entries);
                    report.ingested += 1;
                    if report.ingested.is_multiple_of(25) || total <= 25 {
                        progress(&format!(
                            "  [{}/{}] {} ({} attrs)",
                            next_seq, total, release.release_name, gate.attr_count
                        ));
                    }

                    if aggregator.should_flush() {
                        aggregator.flush(db)?;
                    }
                }
                Err(e) => {
                    progress(&format!("  FAILED {}: {e}", release.release_name));
                    db.mark_release_failed(release.id, &e.to_string())?;
                    report.failed += 1;

                    // Hard-fail fast on configuration errors (e.g. nix
                    // missing for --backfill-evals) instead of failing every
                    // release in the list.
                    if matches!(&e, NxvError::NixEval(msg) if msg.contains("is nix installed")) {
                        hard_error = Some(e);
                        shutdown.store(true, Ordering::SeqCst);
                        break 'outer;
                    }
                }
            }

            if shutdown.load(Ordering::SeqCst) {
                break 'outer;
            }
        }
    }

    // Unblock workers parked in tx.send BEFORE joining them: a worker
    // blocked inside send() never re-checks the shutdown flag and only wakes
    // when the receiver drops — break-then-join without this deadlocks on
    // Ctrl+C and on the hard-error path.
    drop(rx);

    // Final flush of whatever the group holds (also on interrupt — the
    // gated releases in the aggregator are complete observations).
    aggregator.flush(db)?;

    for handle in handles {
        let _ = handle.join();
    }

    if let Some(e) = hard_error {
        return Err(e);
    }
    Ok(())
}

/// `--head-eval`: when every channel observation is stale, evaluate master
/// HEAD directly so the index keeps tracking nixpkgs during channel stalls.
fn run_head_eval(
    db: &mut Database,
    s3: &S3Client,
    _args: &IndexArgs,
    report: &mut RunReport,
    progress: &dyn Fn(&str),
) -> Result<()> {
    let newest = db.newest_ingested_release(None)?;
    let lag_hours = newest
        .as_ref()
        .map(|r| (Utc::now() - r.release_date).num_hours())
        .unwrap_or(i64::MAX);
    if lag_hours < HEAD_EVAL_LAG_HOURS {
        progress(&format!(
            "head-eval skipped: newest channel observation is {lag_hours}h old (< {HEAD_EVAL_LAG_HOURS}h)"
        ));
        return Ok(());
    }

    progress("head-eval: channel observations are stale; evaluating master HEAD...");
    let head = eval::resolve_master_head(s3)?;
    let release_name = format!("master-{}", &head.sha[..12]);

    if !db.insert_release_pending(
        "master",
        &release_name,
        &head.sha,
        None,
        head.committed_at,
        ReleaseSource::HeadEval,
    )? {
        progress(&format!("head-eval: {release_name} already ingested"));
        return Ok(());
    }

    let release = db
        .release_worklist(false)?
        .into_iter()
        .find(|r| r.release_name == release_name)
        .ok_or_else(|| NxvError::Config("head-eval release vanished from ledger".to_string()))?;

    db.mark_release_attempt(release.id)?;
    match eval::ingest_master_head(s3, &head) {
        Ok(entries) => {
            let sentinels = monitor::builtin_sentinels(&[]);
            let gate = monitor::evaluate_release_gate(db, &release, &entries, None, &sentinels)?;
            if !gate.passed() {
                db.mark_release_failed(release.id, &gate.failures.join("; "))?;
                report.failed += 1;
                report.anomalies.push(gate);
                return Ok(());
            }
            let mut aggregator = Aggregator::default();
            aggregator.add_release(&release, entries);
            aggregator.flush(db)?;
            report.ingested += 1;
            progress(&format!(
                "head-eval: ingested {} ({} attrs)",
                release_name, gate.attr_count
            ));
        }
        Err(e) => {
            progress(&format!("head-eval failed: {e}"));
            db.mark_release_failed(release.id, &e.to_string())?;
            report.failed += 1;
        }
    }
    Ok(())
}

/// Build a bloom filter over every distinct attribute path in the database.
///
/// Dotted (nested) attribute paths are inserted verbatim — the query layer
/// checks exact attribute paths against the filter.
pub fn build_bloom_filter(db: &Database) -> Result<PackageBloomFilter> {
    use crate::db::queries;

    let attrs = queries::get_all_unique_attrs(db.connection())?;

    // 1% false-positive rate
    let mut filter = PackageBloomFilter::new(attrs.len().max(1000), 0.01);
    for attr in &attrs {
        filter.insert(attr);
    }

    Ok(filter)
}

/// Build and save a bloom filter for the index.
pub fn save_bloom_filter<P: AsRef<std::path::Path>>(db: &Database, bloom_path: P) -> Result<()> {
    let filter = build_bloom_filter(db)?;
    let bloom_path = bloom_path.as_ref();

    if let Some(parent) = bloom_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    filter.save(bloom_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::tempdir;

    fn planned_release(release_date: DateTime<Utc>, source: ReleaseSource) -> ReleaseRecord {
        ReleaseRecord {
            id: 1,
            channel: "nixos-unstable-small".to_string(),
            release_name: "nixos-20.09pre218523.4a3f9aced7f".to_string(),
            commit_hash: "a".repeat(40),
            commit_count: None,
            release_date,
            source,
            status: crate::db::releases::ReleaseStatus::Pending,
            attempts: 0,
            last_attempt_at: None,
            attr_count: None,
            error: None,
            ingested_at: None,
        }
    }

    /// The work-list filter runs before the fetch-time probe, so a `NixEnv`
    /// guess inside the mixed window must survive it — otherwise the probe that
    /// would reclassify it never runs. ~200 nixos-unstable-small snapshots sat
    /// permanently pending for exactly this reason.
    #[test]
    fn test_needs_eval_admits_releases_after_the_first_artifact() {
        let day = |s: &str| s.parse::<DateTime<Utc>>().unwrap();

        assert!(
            needs_eval(&planned_release(
                day("2019-01-01T00:00:00Z"),
                ReleaseSource::NixEnv
            )),
            "genuinely pre-artifact releases still require --backfill-evals"
        );
        assert!(
            !needs_eval(&planned_release(
                day("2020-04-15T00:00:00Z"),
                ReleaseSource::NixEnv
            )),
            "a date-guessed NixEnv release inside the mixed window must be probed"
        );
        assert!(
            !needs_eval(&planned_release(
                day("2019-01-01T00:00:00Z"),
                ReleaseSource::PackagesJson
            )),
            "an artifact-backed release never needs an evaluation"
        );
    }

    fn test_package(attr: &str, version: &str) -> PackageVersion {
        PackageVersion {
            id: 0,
            name: attr.rsplit('.').next().unwrap_or(attr).to_string(),
            version: version.to_string(),
            first_commit_hash: "a".repeat(40),
            first_commit_date: Utc.timestamp_opt(1_600_000_000, 0).unwrap(),
            last_commit_hash: "b".repeat(40),
            last_commit_date: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            attribute_path: attr.to_string(),
            description: None,
            license: None,
            homepage: None,
            maintainers: None,
            platforms: None,
            source_path: None,
            known_vulnerabilities: None,
        }
    }

    fn entry(attr: &str, version: &str) -> SnapshotEntry {
        SnapshotEntry {
            attribute_path: attr.to_string(),
            name: attr.rsplit('.').next().unwrap_or(attr).to_string(),
            version: version.to_string(),
            description: Some(format!("{attr} description")),
            license: None,
            homepage: None,
            maintainers: None,
            platforms: None,
            source_path: None,
            known_vulnerabilities: None,
        }
    }

    fn release_at(id: i64, secs: i64, hash_char: char) -> ReleaseRecord {
        ReleaseRecord {
            id,
            channel: "nixpkgs-unstable".to_string(),
            release_name: format!("nixpkgs-test-{id}"),
            commit_hash: hash_char.to_string().repeat(40),
            commit_count: Some(id),
            release_date: Utc.timestamp_opt(secs, 0).unwrap(),
            source: ReleaseSource::PackagesJson,
            status: crate::db::releases::ReleaseStatus::Pending,
            attempts: 0,
            last_attempt_at: None,
            attr_count: None,
            error: None,
            ingested_at: None,
        }
    }

    #[test]
    fn test_aggregator_merges_consecutive_releases() {
        let r1 = release_at(1, 1_000, 'a');
        let r2 = release_at(2, 2_000, 'b');
        let r3 = release_at(3, 3_000, 'c');

        let mut agg = Aggregator::default();
        agg.add_release(&r1, vec![entry("firefox", "100.0"), entry("hello", "2.12")]);
        agg.add_release(&r2, vec![entry("firefox", "100.0"), entry("hello", "2.12")]);
        agg.add_release(&r3, vec![entry("firefox", "101.0"), entry("hello", "2.12")]);

        assert_eq!(agg.pairs.len(), 3, "two firefox versions + one hello");
        assert_eq!(agg.releases.len(), 3);

        let ff100 = agg
            .pairs
            .get(&("firefox".to_string(), "100.0".to_string()))
            .unwrap();
        assert_eq!(ff100.first_commit_hash, "a".repeat(40));
        assert_eq!(
            ff100.last_commit_hash,
            "b".repeat(40),
            "100.0 last seen at r2"
        );

        let hello = agg
            .pairs
            .get(&("hello".to_string(), "2.12".to_string()))
            .unwrap();
        assert_eq!(hello.first_commit_hash, "a".repeat(40));
        assert_eq!(
            hello.last_commit_hash,
            "c".repeat(40),
            "hello spans all three"
        );
    }

    #[test]
    fn test_aggregator_flush_marks_releases_ingested() {
        let dir = tempdir().unwrap();
        let mut db = Database::open(dir.path().join("t.db")).unwrap();
        db.insert_release_pending(
            "nixpkgs-unstable",
            "release-1",
            &"a".repeat(40),
            Some(1),
            Utc.timestamp_opt(1_000, 0).unwrap(),
            ReleaseSource::PackagesJson,
        )
        .unwrap();
        let ledger = db.release_worklist(false).unwrap().remove(0);

        let mut agg = Aggregator::default();
        agg.add_release(&ledger, vec![entry("firefox", "100.0")]);
        agg.flush(&mut db).unwrap();
        assert!(agg.is_empty());

        let newest = db.newest_ingested_release(None).unwrap().unwrap();
        assert_eq!(newest.attr_count, Some(1));

        let count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM package_versions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_parse_date_arg() {
        assert_eq!(parse_date_arg(None, "since").unwrap(), None);
        let d = parse_date_arg(Some("2024-01-15"), "since")
            .unwrap()
            .unwrap();
        assert_eq!(d.to_rfc3339(), "2024-01-15T00:00:00+00:00");
        let d = parse_date_arg(Some("2024-01-15T12:30:00Z"), "until")
            .unwrap()
            .unwrap();
        assert_eq!(d.to_rfc3339(), "2024-01-15T12:30:00+00:00");
        assert!(parse_date_arg(Some("yesterday"), "since").is_err());
    }

    #[test]
    fn test_build_bloom_filter_includes_dotted_attrs() {
        let dir = tempdir().unwrap();
        let mut db = Database::open(dir.path().join("test.db")).unwrap();
        db.upsert_observations(&[
            test_package("firefox", "100.0"),
            test_package("python313Packages.requests", "2.32.3"),
        ])
        .unwrap();

        let filter = build_bloom_filter(&db).unwrap();
        assert!(filter.contains("firefox"));
        assert!(filter.contains("python313Packages.requests"));
        assert!(!filter.contains("definitely-not-a-package-xyz"));
    }

    #[test]
    fn test_save_bloom_filter_roundtrip() {
        let dir = tempdir().unwrap();
        let mut db = Database::open(dir.path().join("test.db")).unwrap();
        db.upsert_observations(&[test_package("hello", "2.12")])
            .unwrap();

        let bloom_path = dir.path().join("sub").join("bloom.bin");
        save_bloom_filter(&db, &bloom_path).unwrap();

        let loaded = PackageBloomFilter::load(&bloom_path).unwrap();
        assert!(loaded.contains("hello"));
    }
}
