# Recursive Package Indexing and Parallel Extraction

**Tracking Issue**: [#5 - Add support for nested package sets](https://github.com/jamesbrink/nxv/issues/5)

## Overview

This spec addresses two critical limitations in the current nxv indexer:

1. **Missing nested packages**: Packages under scopes like `qt6.qtwebengine`,
   `python3Packages.numpy`, `haskellPackages.pandoc` are not indexed because
   the extractor only iterates top-level attributes.

2. **Sequential processing**: The indexer processes commits one-at-a-time, modifying the nixpkgs checkout directly, which is slow and prevents parallelization.

### Goals

- Index all derivations in nixpkgs, including those nested under package scopes
- Enable parallel extraction using git worktrees in a temporary directory
- Never modify the user's nixpkgs checkout
- Maintain backward compatibility with existing indexes
- Support graceful shutdown and checkpointing
- Ensure backfill works with nested packages

---

## Phase 1: Worktree Pool Infrastructure

Create a robust worktree management system for parallel extraction.

**Existing code**: `git.rs:96-129` defines `Worktree` struct with `cleanup` flag,
`git.rs:764-830` has `create_worktree()`, `create_worktrees()`, and `remove_worktree()`,
and `git.rs:844-859` has `cleanup_worktrees()` (prunes `nxv-worktree-*` entries).
This phase builds on that foundation.

### 1.1 Worker-Owned Worktree Architecture

**Critical Design Decision**: Workers OWN worktrees rather than borrowing from a pool.

**Problem with pool borrowing**: `WorktreeHandle<'pool>` with a borrow from pool cannot be
moved into worker threads. Rust's ownership rules prevent sending borrowed references across
thread boundaries. Additionally, `git2::Repository` is not `Sync`, so `NixpkgsRepo` cannot be
shared across threads.

**Solution**: Each worker thread creates and owns its worktree at startup:

- [x] Create `src/index/worktree_pool.rs` module (separate from `git.rs` worktree primitives)
- [x] Implement `WorktreeManager` struct (not a pool—no acquire/release)
- [x] **Naming convention (Critical)**: Worktree **basenames** must be `nxv-worktree-{session_id}-{worker_id}`
  - `session_id`: Unique identifier per indexing session (e.g., timestamp or random)
  - Git records the **basename** of the worktree path as the worktree name
  - `cleanup_worktrees()` at `git.rs:844-859` filters `name.starts_with("nxv-worktree-")`
  - Current `create_worktrees()` at `git.rs:803` uses `worker-{i}` which **never matches cleanup**
- [x] Each worker creates its worktree at: `/tmp/nxv-worktrees-{session_id}/nxv-worktree-{session_id}-{worker_id}/`
  - Full path example: `/tmp/nxv-worktrees-abc123/nxv-worktree-abc123-0/`
  - Git sees basename: `nxv-worktree-abc123-0` ✓ matches cleanup prefix
- [x] **Do NOT use existing `create_worktrees()`** - it has wrong naming; use `create_worktree()` directly
- [x] Each worktree is created with `git worktree add --detach`
- [x] Workers own their worktree path for the entire session (no acquire/release)
- [x] Worktrees are reused across commits (checkout new commit, don't recreate)
- [x] On shutdown, each worker cleans up its own worktree via `Drop`

```rust
/// A worktree owned by a worker thread for the duration of the session.
pub struct OwnedWorktree {
    /// Path to the worktree directory.
    pub path: PathBuf,
    /// Path to the main repository (for git commands).
    repo_path: PathBuf,
    /// Current commit checked out (for skip-checkout optimization).
    current_commit: Option<String>,
}

impl OwnedWorktree {
    /// Create a new worktree for this worker.
    /// MUST be called from the main thread before spawning workers.
    pub fn create(repo_path: &Path, session_id: &str, worker_id: usize) -> Result<Self>;

    /// Checkout a commit. Skips if already at this commit.
    pub fn checkout(&mut self, commit_hash: &str) -> Result<()>;
}

impl Drop for OwnedWorktree {
    fn drop(&mut self) {
        // Remove worktree using git -C <repo_path> worktree remove --force <path>
        let _ = Command::new("git")
            .arg("-C").arg(&self.repo_path)  // Critical: specify repo context
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .output();
        let _ = fs::remove_dir_all(&self.path);
    }
}
```

### 1.2 Worker Thread Architecture (Critical)

**Problem**: The original pool-based design with `WorktreeHandle<'pool>` borrows cannot work
because Rust's borrow checker prevents moving borrowed references into threads. Additionally,
`git2::Repository` is `!Sync`, so we cannot share `NixpkgsRepo` across threads.

**Solution**: Workers run as independent processes/threads with owned worktrees:

```rust
/// Configuration for spawning workers.
pub struct WorkerConfig {
    /// Path to the main nixpkgs repository.
    pub repo_path: PathBuf,
    /// Unique session ID for worktree naming.
    pub session_id: String,
    /// Worker's ID (0..num_workers).
    pub worker_id: usize,
    /// Systems to evaluate.
    pub systems: Vec<String>,
}

/// Each worker owns its resources completely.
pub struct Worker {
    /// Owned worktree for this worker.
    worktree: OwnedWorktree,
    /// Worker configuration.
    config: WorkerConfig,
}

impl Worker {
    /// Create a new worker. Worktree is created before the thread spawns.
    pub fn new(config: WorkerConfig) -> Result<Self> {
        let worktree = OwnedWorktree::create(
            &config.repo_path,
            &config.session_id,
            config.worker_id,
        )?;
        Ok(Self { worktree, config })
    }

    /// Process a single commit and return extraction result.
    pub fn process_commit(&mut self, task: CommitTask) -> Result<ExtractionResult> {
        self.worktree.checkout(&task.commit_hash)?;
        // Build file→attr map for THIS commit (no shared state)
        let file_attr_map = build_file_attr_map(&self.worktree.path, &self.config.systems)?;
        // Resolve target attrs from changed paths
        let target_attr_paths = resolve_targets(&task.changed_paths, &file_attr_map)?;
        // Extract packages
        let packages = extract_packages_for_attr_paths(
            &self.worktree.path,
            &self.config.systems[0],
            &target_attr_paths,
        )?;
        Ok(ExtractionResult {
            commit_seq: task.commit_seq,
            packages,
            target_attr_paths,
            changed_paths: task.changed_paths,
        })
    }
}
```

**Key design points**:
- [x] **Do NOT use existing `Worktree` struct** - its auto-cleanup conflicts with our ownership model
- [x] Each worker creates its worktree BEFORE thread spawn (main thread creates worktrees)
- [x] Workers receive ownership of `OwnedWorktree` via move
- [x] Workers rebuild file→attr map per commit (no shared state, no races)
- [x] Workers clean up their worktrees via `Drop` when thread terminates
- [x] Main thread uses channels to send `CommitTask` and receive `ExtractionResult`

**Worker spawn pattern (Critical)**: Workers MUST be created on the main thread, then moved into
spawned threads. This ensures worktree creation happens before thread spawn:

```rust
// CORRECT: Create workers on main thread, then move into spawned threads
let (task_tx, task_rx) = crossbeam_channel::unbounded::<CommitTask>();
let (result_tx, result_rx) = crossbeam_channel::unbounded::<ExtractionResult>();

let mut handles = Vec::new();
for worker_id in 0..num_workers {
    // Worker::new() called on MAIN THREAD - worktree is created here
    let mut worker = Worker::new(WorkerConfig {
        repo_path: repo_path.clone(),
        session_id: session_id.clone(),
        worker_id,
        systems: systems.clone(),
    })?;

    let rx = task_rx.clone();
    let tx = result_tx.clone();
    let shutdown = shutdown_flag.clone();

    // Worker ownership transferred to spawned thread
    handles.push(std::thread::spawn(move || {
        while let Ok(task) = rx.recv() {
            if shutdown.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(result) = worker.process_commit(task) {
                let _ = tx.send(result);
            }
        }
        // Worker::Drop cleans up worktree when thread exits
    }));
}
```

### 1.3 Safety Guarantees

- [x] Original nixpkgs path is **never** modified (read-only after initial clone check)
- [x] All git operations happen on worktrees in temp directory
- [x] Worktree cleanup on panic via `Drop` implementation
- [x] **Startup cleanup**: On session creation, call existing `cleanup_worktrees()` (`git.rs:844-859`)
  to prune stale `nxv-worktree-*` refs from previous crashed runs
- [x] **Orphan detection via lock files** (preferred over PID encoding):
  - Create `/tmp/nxv-worktrees-{session_id}/.lock` when session starts
  - Use `flock(LOCK_EX | LOCK_NB)` to acquire exclusive lock
  - Lock is automatically released when process exits (even on crash)
  - On startup, scan `/tmp/nxv-worktrees-*/`:
    1. Try to acquire lock on each `.lock` file
    2. If lock succeeds → orphan directory, safe to delete
    3. If lock fails → active session, skip
  - This is more reliable than PID checking (PIDs can wrap)

### 1.4 Signal Handling (Critical)

**Problem**: The original spec proposed registering SIGTERM/SIGINT handlers in the pool, but
`main.rs:1119` and `main.rs:1217` already install global `ctrlc` handlers. Multiple handlers
conflict and cleanup in a signal handler is unsafe (allocations, locks, etc.).

**Solution**: Reuse the existing shutdown infrastructure:

- [x] **Do NOT install new signal handlers** in the worktree manager or workers
- [x] Use the existing `Indexer::shutdown_flag()` (`mod.rs:574-576`)
- [x] Workers check `shutdown.load(Ordering::SeqCst)` periodically
- [x] On shutdown request, workers:
  1. Complete their current commit (if any) or abandon gracefully
  2. Send a final result or abort message to the main thread
  3. Let `Drop` handle worktree cleanup naturally
- [x] Main thread:
  1. Stops dispatching new commits
  2. Waits for in-flight workers with timeout
  3. Saves checkpoint at last fully-processed commit
  4. Worker `Drop` implementations clean up worktrees

```rust
// In worker loop:
while let Ok(task) = receiver.recv() {
    if shutdown.load(Ordering::SeqCst) {
        // Send abort message and exit loop cleanly
        break;
    }
    let result = self.process_commit(task)?;
    sender.send(result)?;
}
// OwnedWorktree::Drop handles cleanup automatically
```

### 1.5 Success Criteria

- [x] Unit tests for `OwnedWorktree` creation and cleanup
- [x] Test that original nixpkgs is never modified during extraction
- [x] Test worktree reuse across multiple checkouts in the same worker
- [x] Test cleanup happens on normal exit and panic
- [x] Integration test: spawn 4 workers, process 10 commits each, verify all worktrees cleaned up
- [x] Test that existing shutdown flag (`Indexer::shutdown_flag()`) properly signals workers

---

## Phase 2: Recursive Package Discovery

Modify the Nix extractor to discover and extract nested package scopes.

### 2.1 Identify Recursable Package Sets

- [x] Create list of known package scope patterns to recurse into:

  ```text
  - *Packages (python3Packages, haskellPackages, perlPackages, etc.)
  - qt5, qt6
  - libsForQt5
  - gnome, xfce, mate, pantheon, plasma5Packages
  - nodePackages, nodePackages_latest
  - ocamlPackages
  - beam.packages.* (erlang)
  - rPackages
  - vimPlugins, emacsPackages
  - fishPlugins, zshPlugins
  - linuxPackages_*
  ```

- [x] Detect `recurseForDerivations = true` attribute dynamically
- [x] Configure maximum recursion depth (default: 2) to prevent infinite loops

### 2.2 Update EXTRACT_NIX Expression

- [x] Modify `EXTRACT_NIX` in `extractor.rs` to accept a `recurse` parameter
- [x] When `recurse = true`:
  - Check each attribute for `recurseForDerivations = true`
  - If found, recursively extract derivations with prefixed attribute path
  - Store full attribute path (e.g., `qt6.qtwebengine` not `qtwebengine`)
- [x] Add recursion depth tracking to prevent stack overflow
- [x] Handle cycles in package sets (some sets reference each other)

### 2.3 Dotted Attribute Path API (Critical)

**Problem**: Current `EXTRACT_NIX` only accepts top-level attribute names. The expression
uses `builtins.attrNames pkgs` and `pkgs ? name` checks (`extractor.rs:234-264`), which
fail for dotted paths like `python3Packages.numpy`. Incremental indexing and backfill
pass attribute lists directly to the extractor, so they cannot selectively update nested
packages without this change.

**Solution**: Extend the extractor API to support multi-segment attribute paths:

- [x] Add new `extract_packages_for_attr_paths()` function signature:

  ```rust
  pub fn extract_packages_for_attr_paths<P: AsRef<Path>>(
      repo_path: P,
      system: &str,
      attr_paths: &[AttrPath],  // e.g., [AttrPath(["python3Packages", "numpy"]), ...]
  ) -> Result<Vec<PackageInfo>>
  ```

- [x] Create `AttrPath` type that handles multi-segment paths:

  ```rust
  /// Represents a full attribute path like "beam.packages.erlang_26.rebar3"
  /// Stored as a list of segments for unambiguous representation.
  #[derive(Clone, Debug, PartialEq, Eq, Hash)]
  pub struct AttrPath(Vec<String>);

  impl AttrPath {
      /// Parse from dotted string: "a.b.c" -> ["a", "b", "c"]
      pub fn parse(s: &str) -> Self { Self(s.split('.').map(String::from).collect()) }
      /// Create from segments directly
      pub fn from_segments(segments: Vec<String>) -> Self { Self(segments) }
      /// Get segments
      pub fn segments(&self) -> &[String] { &self.0 }
      /// Top-level attr (single segment)
      pub fn is_top_level(&self) -> bool { self.0.len() == 1 }
      /// Convert back to dotted string for display/database storage
      pub fn to_dotted(&self) -> String { self.0.join(".") }
  }
  ```

- [x] **JSON serialization** (for passing to Nix):
  - Rust → JSON: `[["beam", "packages", "erlang_26"], ["python3Packages", "numpy"]]`
  - This is list-of-lists format, unambiguous for paths with dots in segment names
  - Nix reads as `attrPaths` and uses `builtins.getAttrByPath` directly
  - **Escaping rules**: Use `serde_json::to_string()` which handles:
    - Quotes in attr names: `"foo\"bar"` → `"foo\"bar"` (escaped)
    - Backslashes: `"foo\\bar"` → `"foo\\bar"` (escaped)
    - Unicode: Preserved as-is (JSON is UTF-8)
    - Nixpkgs attr names are typically `[a-zA-Z0-9_-]` so escaping is rarely needed

- [x] Update `EXTRACT_NIX` to accept `attrPaths` parameter (list of segment lists):

  ```nix
  # attrPaths: [["beam", "packages", "erlang_26", "rebar3"], ["python3Packages", "numpy"]]
  { nixpkgsPath, system, attrPaths ? null }:
  let
    # Navigate path segments using builtins
    getAttrByPath = builtins.foldl' (acc: seg: acc.${seg});
    hasAttrByPath = path: set:
      builtins.foldl' (acc: seg:
        acc && (if builtins.isAttrs (getAttrByPath (builtins.head path) set)
                then builtins.hasAttr seg (getAttrByPath (builtins.head path) set)
                else false)
      ) true path;
  in ...
  ```

- [x] Keep `extract_packages_for_attrs()` as convenience wrapper for top-level only
- [x] Update call sites:
  - `mod.rs:982-984` → use `extract_packages_for_attr_paths()`
  - `backfill.rs:249-250` → use `extract_packages_for_attr_paths()`
  - `backfill.rs:422-423` → use `extract_packages_for_attr_paths()`
- [x] **NOTE**: The spec previously referenced `extract_packages_for_scoped_attrs()` which does
  not exist. Use `extract_packages_for_attr_paths()` consistently throughout.

**Why not `(Option<String>, String)` or dotted strings?**

- Real nixpkgs paths go 3+ levels deep: `beam.packages.erlang_26.rebar3`
- Some attr names contain dots (rare but possible)
- `Vec<String>` representation is unambiguous and handles arbitrary depth

### 2.4 Change Detection for Nested Packages (Critical)

**Problem**: `unsafeGetAttrPos` (`extractor.rs:269-322` POSITIONS_NIX, line 294) reports where an attribute is *defined*
in the parent scope (e.g., `python-packages.nix`), NOT where the package implementation lives
(e.g., `pkgs/development/python-modules/numpy/default.nix`). This means file changes to nested
packages won't trigger updates because the file→attr map points to the wrong locations.

**Architectural Decision**: Use `meta.position`/`source_path` based mapping instead of `unsafeGetAttrPos`.

The existing `EXTRACT_NIX` already extracts `meta.position` via `getSourcePath` (`extractor.rs:187-209`),
which correctly resolves to the package's actual definition file. While this requires evaluating
each package (slower), it provides accurate file mapping essential for incremental indexing.

**Solution**: Build file→attr map from package extraction, not position introspection:

- [x] Create `POSITIONS_FROM_META_NIX` expression that:
  - Evaluates each package (top-level and nested scopes)
  - Extracts `meta.position` to get the actual source file
  - Returns `{ attrPath = "python3Packages.numpy"; file = "pkgs/development/python-modules/numpy/default.nix"; }`
- [x] **Do NOT rely on `unsafeGetAttrPos`** for nested packages—it gives wrong files
- [x] Update `build_file_attr_map()` to use the meta.position-based approach
- [x] File→attr map keys are relative file paths; values are full dotted attr paths
- [x] Target path resolution in `mod.rs:935-966` works unchanged (file lookup returns dotted path)
- [x] Workers build this map per-commit (no stale map issues in parallel mode)

**Performance trade-off**: `meta.position` requires evaluating packages (slower than `unsafeGetAttrPos`).
Mitigation strategies:

- Workers build maps per-commit in parallel (amortized across workers)
- Cache position maps by commit hash if needed for very large indexes
- Only extract positions for whitelisted scopes to limit scope

**Why not `unsafeGetAttrPos`**: For a package like `python3Packages.numpy`:

- `unsafeGetAttrPos "numpy" python3Packages` → returns `python-packages.nix:12345`
- `pkgs.python3Packages.numpy.meta.position` → returns `pkgs/development/python-modules/numpy/default.nix:1`

The latter is what we need for change detection.

### 2.5 File Map Refresh for Nested Scopes (Critical)

**Problem**: The current `should_refresh_file_map()` at `mod.rs:1185-1197` relies on stateful
logic (`mapping_commit` at `mod.rs:840`) that assumes sequential commit processing. In parallel
mode, workers processing commits out of order would use stale maps, leading to missed updates.

Additionally, the trigger list only includes top-level files:

```rust
const TOP_LEVEL_FILES: [&str; 4] = [
    "pkgs/top-level/all-packages.nix",
    "pkgs/top-level/default.nix",
    "pkgs/top-level/aliases.nix",
    "pkgs/top-level/impure.nix",
];
```

Nested scope definitions (`python-packages.nix`, etc.) are not checked.

**Architectural Decision**: Workers rebuild file→attr map per commit (Option B).

This is slower but eliminates all stale map issues and race conditions in parallel mode.
Since workers already evaluate packages to extract metadata, the marginal cost of also
extracting positions is acceptable.

**Solution**: Per-commit map rebuild in workers:

- [ ] **Remove** stateful `mapping_commit` tracking from main indexer (kept for sequential mode)
- [ ] **Remove** `should_refresh_file_map()` check (kept for sequential mode when < 10 commits)
- [x] Each worker calls `build_file_attr_map()` in its worktree for its current commit
- [x] Map is computed fresh, contains only packages that exist at that commit
- [x] `target_attr_paths` is resolved locally by the worker, then sent in `ExtractionResult`
- [x] Main thread receives resolved targets—no shared map state

```rust
// In Worker::process_commit():
fn process_commit(&mut self, task: CommitTask) -> Result<ExtractionResult> {
    self.worktree.checkout(&task.commit_hash)?;

    // Always rebuild map for this commit (no shared state, no races)
    let file_attr_map = build_file_attr_map(&self.worktree.path, &self.config.systems)?;

    // Resolve targets using freshly built map
    let target_attr_paths = resolve_targets(&task.changed_paths, &file_attr_map)?;

    // Extract packages
    let packages = extract_packages_for_attr_paths(...)?;

    Ok(ExtractionResult {
        commit_seq: task.commit_seq,
        packages,
        target_attr_paths,  // Resolved by worker
        changed_paths: task.changed_paths,
    })
}
```

**Why not Option A (extend triggers)?** Even with an extended trigger list, parallel workers
would still race on a shared map. Per-commit rebuild is the only safe approach for parallelism.

### 2.6 Attribute Path Handling

- [x] Store full dotted attribute path in database (already supported by schema)
- [x] Ensure `name` field remains the package name (pname), not the full path
- [x] Update bloom filter to include full attribute paths
- [x] Verify search still works for both name and attribute path queries

### 2.7 Extraction Performance

- [ ] Profile extraction time with recursion enabled
- [x] Add `--no-recurse` flag to skip nested packages (for faster incremental updates)
- [ ] Consider caching which scopes exist at each commit to avoid re-evaluation

### 2.8 Success Criteria

- [x] Unit test: extract from mock nixpkgs with nested derivation, verify full path stored
- [ ] Integration test: extract `qt6.qtwebengine` from real nixpkgs HEAD
- [ ] Integration test: extract `python3Packages.numpy` from real nixpkgs HEAD
- [x] Test that deeply nested packages (>2 levels) are handled correctly
- [ ] Benchmark: extraction time increase is <3x compared to top-level only
- [x] Test: `extract_packages_for_attr_paths()` correctly extracts `AttrPath(["python3Packages", "numpy"])`
- [ ] Test: file→attr map includes `python3Packages.numpy` for `pkgs/development/python-modules/numpy/default.nix`
- [ ] Test: incremental index detects change to nested package file and updates correctly

---

## Phase 3: Parallel Indexer Architecture

Refactor the indexer to use worker threads with worktree pool.

**Sequential vs Parallel Mode**: The indexer uses two code paths:
- **Parallel mode** (`process_commits_parallel()`): Used when processing ≥10 commits. Uses worktrees
  and worker threads for safe, parallel extraction. This is the primary path for full indexing.
- **Sequential mode** (`process_commits()`): Used when processing <10 commits. Uses direct checkout
  on the nixpkgs repository. Retained for backward compatibility and fast incremental updates.

The threshold of 10 commits balances worktree creation overhead against parallelization benefits.
For very small updates, sequential mode avoids the ~1-2 second worktree setup time.

**Call-site migration required**: Currently `process_commits()` (`mod.rs:778-1139`) and
`run_backfill_historical()` (`backfill.rs:321-483`) directly call `repo.checkout_commit()`
on the user's nixpkgs. These must be refactored to use `WorktreeHandle` exclusively.

### 3.1 Indexer Refactoring

- [x] Extract commit processing logic into standalone `process_single_commit()` function
  - Worker processing logic at `mod.rs:1303-1371`
- [x] Create `IndexerWorker` struct that owns a `WorktreeHandle`
  - `Worker` struct at `worktree_pool.rs:426`
- [x] Worker receives commits via channel, processes them, sends results back
  - Uses `crossbeam_channel` at `mod.rs:1266-1267`
- [x] Main thread coordinates workers and handles database writes
  - Coordination at `mod.rs:1408-1586`
- [x] **Remove** all `repo.checkout_commit()` calls from parallel indexer code path
  - Sequential path (`process_commits()`) still uses checkout for backward compatibility
- [x] **Migrate call sites** (parallel mode):
  - `mod.rs:1278` - worktree created per worker
  - `mod.rs:1305` - worker handles checkout via `OwnedWorktree`
  - `mod.rs:1307-1313` - file map built in worker's worktree
  - `backfill.rs:336` - historical checkout uses `WorktreeSession`

### 3.2 Responsibility Contract

**Main thread responsibilities**:

- Maintains the commit queue (ordered by date)
- Dispatches commit+target_attrs tuples to workers
- Receives `ExtractionResult` from workers
- Buffers out-of-order results in `pending_results: BTreeMap<CommitSeq, ExtractionResult>`
- Processes results in sequence order for open-range bookkeeping (`mod.rs:1010-1072`)
- Writes to database (single writer, no contention)
- Handles checkpointing and graceful shutdown

**Database batching strategy**:

- [ ] Batch size: 100 commits per transaction (configurable via `--batch-size`)
- [ ] Use SQLite transactions: `BEGIN IMMEDIATE` → inserts → `COMMIT`
- [ ] On transaction failure:
  1. Roll back the failed batch
  2. Retry the batch once with smaller size (50)
  3. If still fails, fall back to single-commit transactions
  4. Log the error but continue processing
- [ ] Checkpoint after each successful batch (not per-commit)
- [ ] On graceful shutdown, commit partial batch before exit
- [ ] Use `INSERT OR REPLACE` for idempotent inserts (safe for restart)
- [ ] WAL mode already enabled in `db/mod.rs` for better concurrent read performance

**Worker thread responsibilities**:

- Owns a `WorktreeHandle` from the pool
- Receives `(commit_hash, target_attrs, commit_seq)` from channel
- Calls `handle.checkout(commit_hash)`
- Computes `changed_paths` via `git diff-tree` in worktree
- Builds file→attr map if needed (in worktree, not main repo)
- Resolves `target_attr_paths` from changed files
- Runs extraction via `extract_packages_for_attr_paths()`
- Sends `ExtractionResult { commit_seq, packages, target_attr_paths, changed_paths }` back

### 3.3 ExtractionResult Type (Critical)

**Problem**: The original payload `{ commit_seq, packages, changed_paths }` omits `target_attr_paths`.
The range bookkeeping in `mod.rs:1056-1072` uses `target_attr_paths` to detect when packages
disappear (present in targets but not in extraction results). Without this, the main thread
cannot safely close ranges for deletions.

**Solution**: Include `target_attr_paths` in the result:

```rust
pub struct ExtractionResult {
    /// Sequence number for ordering
    pub commit_seq: u64,
    /// Extracted packages
    pub packages: Vec<PackageInfo>,
    /// Attr paths we attempted to extract (for deletion detection)
    pub target_attr_paths: HashSet<String>,
    /// Files changed in this commit
    pub changed_paths: Vec<String>,
}
```

The main thread uses `target_attr_paths` to:

1. Identify packages that were targeted but not returned (deleted/renamed)
2. Close open ranges for those packages with `last_commit = prev_commit`
3. This logic remains unchanged from `mod.rs:1056-1072`

### 3.4 Parallel Commit Processing

- [x] Add `--workers N` CLI option (default: number of CPUs / 2, min 1)
  - Implemented at `cli.rs:337-338`, auto-detection at `mod.rs:1202-1209`
- [x] Distribute commits across workers using bounded channel
  - Uses `crossbeam_channel::bounded` at `mod.rs:1266`
- [x] Each worker:
  1. Owns worktree for session duration (`mod.rs:1278`)
  2. Checks out assigned commit (`mod.rs:1305`)
  3. Builds file→attr map in worktree (`mod.rs:1307-1313`)
  4. Runs extraction with recursion settings (`mod.rs:1348-1361`)
  5. Sends `ExtractionResult` back via channel (`mod.rs:1363-1375`)
- [x] Main thread:
  1. Receives results (`mod.rs:1459-1463`)
  2. Buffers out-of-order results in `BTreeMap` (`mod.rs:1409,1466`)
  3. Processes results in sequence order (`mod.rs:1470-1586`)
  4. Uses `target_attr_paths` for deletion detection (`mod.rs:1543-1559`)
  5. Batches database writes at checkpoint intervals (`mod.rs:1571-1584`)

### 3.5 Ordering Guarantees

- [x] Commits must be processed in chronological order for range tracking
  - Results processed in sequence via `next_seq` counter (`mod.rs:1410,1470`)
- [x] Use commit sequence numbers to ensure correct ordering
  - `commit_seq` in `CommitTask` and `ExtractionResult`
- [x] Buffer out-of-order results until preceding commits complete
  - `pending_results: BTreeMap<u64, ExtractionResult>` at `mod.rs:1409`
- [x] Checkpoint only after all preceding commits are processed
  - Checkpoint inside ordered processing loop (`mod.rs:1571-1584`)

### 3.6 Graceful Shutdown and Panic Recovery

- [x] Signal workers to stop on Ctrl+C
  - Workers check `shutdown.load(Ordering::SeqCst)` at `mod.rs:1292`
- [x] Wait for in-flight extractions to complete (with timeout)
  - Uses `recv_timeout(100ms)` at `mod.rs:1459`
- [x] Checkpoint at last fully-processed commit
  - Checkpoint on interrupt at `mod.rs:1617-1626`
- [x] Clean up all worktrees before exit
  - `OwnedWorktree::Drop` at `worktree_pool.rs:139-153`

**Worker panic recovery strategy**:

- [x] Use `std::thread::spawn` which returns a `JoinHandle` that can detect panics
  - Handles collected at `mod.rs:1289`, joined at `mod.rs:1589-1591`
- [ ] Main thread periodically checks if workers are alive via `JoinHandle::is_finished()`
- [x] If a worker panics:
  1. Its `OwnedWorktree::Drop` still runs (Rust guarantees this during unwinding)
  2. Empty result sent for the commit (`mod.rs:1388-1397`)
  3. The commit is processed with empty packages (safe: can be re-indexed later)
  4. (Not implemented) Replacement worker spawning
  5. (Not implemented) Abort on too many failures
- [x] Use `std::panic::catch_unwind` inside worker loop for non-fatal panics:
  - Implemented at `mod.rs:1303-1399`
- [x] Worker panics should NOT crash the entire indexing process
  - Empty result sent, processing continues

### 3.7 Progress Reporting

- [ ] Per-worker progress display (not implemented, single progress bar used)
- [x] Overall ETA based on commits remaining
  - `EtaTracker` with sliding window at `mod.rs:1233`
- [x] Show current commit being processed by each worker
  - Progress bar shows current commit hash/date at `mod.rs:1472-1482`

### 3.8 Success Criteria

- [ ] Test parallel extraction with 4 workers processes commits correctly
- [ ] Test ordering is maintained when workers complete out-of-order
- [ ] Test graceful shutdown saves valid checkpoint
- [ ] Test that database is consistent after parallel indexing
- [ ] Benchmark: 4 workers should be ~3x faster than single-threaded (I/O bound)
- [x] Test: no `checkout_commit()` calls remain in parallel indexer code path
  - Parallel path uses worktrees exclusively; sequential path retained for compatibility
- [ ] Test: original nixpkgs working directory unchanged after parallel indexing
- [x] Test: `ExtractionResult.target_attr_paths` correctly enables deletion detection
  - Used at `mod.rs:1543-1559` for range closing

---

## Phase 4: Backfill Enhancement

Update backfill to work with nested packages and use worktrees.

**Current issue**: Both `run_backfill_head()` (`backfill.rs:172-300`) and
`run_backfill_historical()` (`backfill.rs:321-483`) call `extract_packages_for_attrs()`
which doesn't support dotted paths like `python3Packages.numpy`.

### 4.1 Head-Mode Backfill for Nested Attrs (Critical)

**Problem**: Head-mode backfill (the default, no `--history` flag) at `backfill.rs:249-250`
calls `extract_packages_for_attrs()` with attribute paths from the database. When
nested packages are indexed (e.g., `python3Packages.numpy`), this extraction fails
silently because the extractor can't resolve dotted paths.

**Solution**: Update head-mode backfill to use the new path-based API:

- [x] Change `backfill.rs:249-250` from `extract_packages_for_attrs()` to `extract_packages_for_attr_paths()`
- [x] Parse `attr_list` entries into `AttrPath` instances before extraction
- [x] No worktree needed for head-mode (extracts from current HEAD only)
- [ ] Test: backfill correctly fills `source_path`/`homepage` for `python3Packages.numpy`

### 4.2 Historical-Mode Nested Package Support

- [x] Update `get_attrs_needing_backfill()` to return full attribute paths
- [x] Parse dotted paths into `AttrPath` instances for extraction
- [x] Use `extract_packages_for_attr_paths()` instead of `extract_packages_for_attrs()`
- [x] Update `backfill.rs:422-423` call site to use path-based API
- [ ] Verify backfill correctly updates nested package records

### 4.3 Worktree-Based Historical Backfill

- [x] Refactor `run_backfill_historical()` to use `WorktreePool`
- [x] **Remove** `repo.checkout_commit()` call at `backfill.rs:412`
- [x] **Remove** `repo.restore_ref()` call at `backfill.rs:404,472-475` (no longer needed)
- [x] Acquire `WorktreeHandle` at start of backfill, release on completion
- [x] Never modify the main nixpkgs checkout
- [ ] Support parallel backfill with multiple worktrees for different commits

### 4.4 Historical Mode Improvements

- [ ] Group commits by proximity to minimize checkout thrashing
- [ ] Use worktree pool for parallel commit processing
- [ ] Add progress indicator for commit traversal

### 4.5 Success Criteria

- [ ] Test backfill populates metadata for `qt6.qtwebengine`
- [ ] Test head-mode backfill fills metadata for `python3Packages.numpy`
- [ ] Test backfill with `--history` mode uses worktrees
- [ ] Test original nixpkgs is unmodified after backfill
- [ ] Integration test: backfill missing source_path for nested packages
- [ ] Test: no `checkout_commit()` or `restore_ref()` calls in backfill code path
- [x] Test: `AttrPath::parse("beam.packages.erlang_26.rebar3")` returns 4 segments

---

## Phase 5: CLI and Configuration Updates

### 5.1 New CLI Options

- [x] `--workers N` - Number of parallel workers (default: auto-detect)
- [x] `--no-recurse` - Skip nested package scopes (faster for updates)
- [x] `--recurse-all` - Use dynamic detection instead of whitelist
- [x] `--recurse-depth N` - Maximum recursion depth (default: 2)
- [x] `--worktree-dir PATH` - Custom worktree directory (default: system temp)

### 5.2 Configuration Validation

- [ ] Validate nixpkgs path is a valid git repository
- [ ] Check available disk space for worktrees (warn if < N_workers × 2GB; each worktree is ~1.5GB)
- [ ] Verify git version supports worktrees (>= 2.5)

### 5.3 Status and Diagnostics

- [ ] Add `nxv index status` command showing:
  - Last indexed commit
  - Number of packages (top-level vs nested)
  - Worktree status (if any exist)
- [ ] Add `--verbose` flag for detailed extraction logging

### 5.4 Success Criteria

- [ ] CLI help text documents new options
- [ ] Invalid options produce clear error messages
- [ ] Status command shows accurate nested package counts

---

## Phase 6: Database and Compatibility

### 6.1 Schema Compatibility

- [ ] Existing schema already supports dotted `attribute_path` - verify no changes needed
- [ ] Add database migration if schema version bump is required
- [ ] Ensure old indexes can be read (backward compatibility)

### 6.2 Index Metadata Updates

- [x] Store `recursive_extraction = true/false` in meta table
- [x] Store `recurse_all = true/false` in meta table
- [x] Store `recursion_depth` in meta table
- [x] Store `worktree_parallel = true/false` in meta table

### 6.3 Bloom Filter Updates

- [ ] Include full attribute paths in bloom filter
- [ ] Verify search performance with larger bloom filter
- [ ] Consider separate bloom filter for nested packages (optimization)

### 6.4 Success Criteria

- [ ] Old index files can be read by new code
- [ ] New index files can be read by old code (graceful degradation)
- [ ] Bloom filter correctly rejects non-existent nested packages

---

## Phase 7: Testing and Validation

### 7.1 Unit Tests

- [ ] `worktree.rs`: Pool creation, acquire/release, cleanup
- [ ] `extractor.rs`: Recursive extraction Nix expression
- [ ] `mod.rs`: Parallel worker coordination, ordering

### 7.2 Integration Tests

- [ ] Full index of test nixpkgs subset with nested packages
- [ ] Incremental index adds new nested packages correctly
- [ ] Backfill updates nested package metadata
- [ ] Search finds nested packages by name and attribute path

### 7.3 End-to-End Tests

- [ ] Index real nixpkgs HEAD with recursion, verify qt6.qtwebengine exists
- [ ] Index with 4 workers, verify identical results to single-threaded
- [ ] Simulate Ctrl+C during indexing, verify clean recovery

### 7.4 Performance Tests

- [ ] Benchmark: single-threaded vs 2/4/8 workers
- [ ] Benchmark: with recursion vs without
- [ ] Memory usage profiling with large worktree pool
- [ ] Disk space usage with multiple worktrees

### 7.5 Success Criteria

- [ ] All existing tests pass
- [ ] New tests achieve >80% coverage of new code
- [ ] No regressions in search performance
- [ ] CI pipeline runs full test suite

---

## Implementation Order

1. **Phase 1** (Worktree Pool) - Foundation for all other changes
2. **Phase 2** (Recursive Discovery) - Core feature, can test with single thread
3. **Phase 6** (Database) - Ensure schema works before parallel writes
4. **Phase 3** (Parallel Indexer) - Biggest refactor, depends on 1 & 2
5. **Phase 4** (Backfill) - Uses infrastructure from 1 & 3
6. **Phase 5** (CLI) - Polish and configuration
7. **Phase 7** (Testing) - Ongoing throughout, final validation

---

## Risks and Mitigations

| Risk                                         | Mitigation                                      |
| -------------------------------------------- | ----------------------------------------------- |
| Worktree creation is slow                    | Reuse worktrees, create pool upfront            |
| Disk space exhaustion                        | Monitor usage, configurable pool size, cleanup  |
| Nix evaluation hangs                         | Timeout per extraction, kill worker process     |
| Database contention                          | Single writer thread, batch inserts             |
| Memory pressure from large results           | Stream results, limit batch sizes               |
| Inconsistent results from parallel execution | Strict ordering, deterministic merge            |
| Parallel nix eval memory exhaustion          | Limit workers, monitor RSS, see guidance below  |

### Memory Limit Guidance for Parallel Nix Eval

Running N parallel `nix eval` processes can consume significant memory. Each evaluation of
nixpkgs loads a substantial portion of the expression tree.

**Recommendations**:

- [ ] **Default worker count**: `min(num_cpus / 2, 4)` to balance parallelism vs memory
- [ ] **Memory per worker**: Expect ~2-4GB peak RSS per `nix eval` on full nixpkgs
- [ ] **System requirements**: Recommend 16GB RAM for 4 workers, 8GB for 2 workers
- [ ] **CLI flag**: `--max-memory-per-worker` to set soft limit (requires external tooling)
- [ ] **Monitoring**: Log peak RSS per worker for tuning guidance
- [ ] **Graceful degradation**: If system memory pressure detected (via `/proc/meminfo`),
      reduce active workers temporarily or pause new task dispatch
- [ ] **Nix-specific**: Consider `--option max-jobs 1` in nix eval to prevent nested parallelism
- [ ] **OOM killer protection**: Workers should be lower priority than main thread;
      main thread checkpoints on worker OOM death

---

## Design Decisions

### 1. Whitelist vs Dynamic Detection

**Decision**: Use a curated whitelist by default, with `--recurse-all` flag for dynamic detection.

**Rationale**: Whitelist is safer and covers 99% of use cases. Dynamic detection via
`recurseForDerivations = true` can hit edge cases (infinite recursion, broken evaluations).
Power users can opt into full dynamic detection.

**Default whitelist**:

```text
*Packages (python3Packages, haskellPackages, perlPackages, rubyPackages, etc.)
qt5, qt6, libsForQt5
gnome, xfce, mate, pantheon, cinnamon
plasma5Packages, kdePackages
nodePackages, nodePackages_latest
ocamlPackages, coqPackages
beam.packages.erlang*
rPackages
vimPlugins, emacsPackages
fishPlugins, zshPlugins, tmuxPlugins
linuxPackages, linuxPackages_*
```

### 2. Recursion Depth

**Decision**: Default depth of 2, configurable via `--recurse-depth N`.

**Rationale**: Most ecosystem packages are 1 level deep (`python3Packages.numpy`).
Some like `linuxPackages_6_6.nvidia_x11` are 2 levels. Depth 3+ is rare and risks
performance issues. Users needing deeper can specify explicitly.

### 3. Search Ranking

**Decision**: No special ranking for nested vs top-level packages.

**Rationale**: This matches [NixOS search](https://github.com/NixOS/nixos-search) behavior.
They use Elasticsearch with field boosting:

| Field              | Boost |
| ------------------ | ----- |
| `attr_name`        | 9.0   |
| `pname`            | 6.0   |
| `description`      | 1.3   |

Searching "numpy" matches `pname` (6.0 boost), while "python3Packages.numpy" matches
`attr_name` (9.0 boost). This naturally handles both use cases without special logic.

### 4. Incremental Recursion

**Decision**: Yes, support adding nested packages to existing non-recursive indexes.

**Implementation**: When `--recurse` is enabled on an existing index:

1. Extract nested packages from HEAD (or specified commit range)
2. Insert new records with `first_commit = last_commit = extraction_commit`
3. Future incremental runs will track these packages normally
4. Historical data for nested packages only goes back to when recursion was enabled

This allows gradual adoption without requiring a full re-index of all commits.

---

## Appendix: Review Findings and Resolutions

This section documents issues identified during spec review and how they are addressed.

### Review Round 1 (Initial)

#### High Priority

##### 1. EXTRACT_NIX doesn't support dotted paths for filtered extraction

**Issue**: Current `EXTRACT_NIX` uses `builtins.attrNames pkgs` and `pkgs ? name` checks
(`extractor.rs:234-264`), which fail for dotted paths. Incremental/backfill pass attr
lists directly, so they cannot selectively update nested packages.

**Resolution**: Added **Section 2.3 (Dotted Attribute Path API)** with:

- New `extract_packages_for_attr_paths()` function
- `AttrPath` type with `Vec<String>` segments for multi-level paths
- Updated Nix expression using `builtins.hasAttrByPath` and `getAttrByPath`
- Call-site migration plan for `mod.rs:982-984` and `backfill.rs:249-250,422-423`

##### 2. Change detection is top-level only

**Issue**: File→attr map built from `unsafeGetAttrPos` over top-level names only
(`extractor.rs:269-322` POSITIONS_NIX). Edits to nested package files won't trigger updates.

**Resolution**: Added **Section 2.4 (Change Detection for Nested Packages)** with:

- `POSITIONS_RECURSIVE_NIX` expression for nested scopes
- Updated `build_file_attr_map()` to use recursive positions
- File paths correctly map to full dotted attr paths

#### Medium Priority

##### 3. "Never modify nixpkgs" needs call-site migration

**Issue**: `process_commits()` and `run_backfill_historical()` call `checkout_commit()`
on user's nixpkgs. Adding worktree pool doesn't help without refactoring these paths.

**Resolution**: Added explicit migration in:

- **Section 3.1**: Lists specific call sites to migrate (`mod.rs:838,900,927-933`, `backfill.rs:412`)
- **Section 3.2**: Defines responsibility contract (workers own worktrees, main thread never checkouts)
- **Section 4.3**: Backfill-specific migration with `checkout_commit()` and `restore_ref()` removal

##### 4. Worktree lifecycle/cleanup under-specified

**Issue**: Spec proposed `/tmp/nxv-worktrees-{pid}/` without naming rules or startup
pruning. Misalignment with existing `cleanup_worktrees()` at `git.rs:844-859`.

**Resolution**: Updated **Section 1.1** with:

- Naming convention: `nxv-worktree-{pool_id}-{worker_id}` (aligns with existing prefix matching)
- Uses existing `NixpkgsRepo::create_worktree()` internally
- Updated **Section 1.3** with startup cleanup via existing `cleanup_worktrees()`
- Orphan detection for temp directories

##### 5. Parallel architecture responsibilities unclear

**Issue**: Unspecified whether main thread or workers compute `changed_paths`/`target_attr_paths`.
Open-range bookkeeping is order-sensitive.

**Resolution**: Added **Section 3.2 (Responsibility Contract)** with:

- Main thread: commit queue, result buffering, ordering, DB writes
- Workers: checkout, changed_paths, target resolution, extraction
- Explicit `ExtractionResult` type with `commit_seq` for ordering
- `pending_results: BTreeMap<CommitSeq, ExtractionResult>` for buffering

---

### Review Round 2 (Codex)

#### High Priority (Round 2)

##### 6. Result payload misses target attr set

**Issue**: `ExtractionResult` only had `{ commit_seq, packages, changed_paths }`. The range
bookkeeping at `mod.rs:1056-1072` uses `target_attr_paths` to detect when packages disappear.
Without returning resolved attr paths, main thread cannot close ranges for deletions.

**Resolution**: Added **Section 3.3 (ExtractionResult Type)** with:

- `target_attr_paths: HashSet<String>` field in `ExtractionResult`
- Main thread uses this for deletion detection (packages targeted but not returned)
- Range closing logic remains at `mod.rs:1056-1072`

##### 7. ScopedAttr shape can't represent real paths

**Issue**: Original `(Option<String>, String)` only allows one scope segment. Real nixpkgs
paths go 3+ levels deep: `beam.packages.erlang_26.rebar3`, `linuxPackages_6_6.nvidia_x11`.

**Resolution**: Replaced `ScopedAttr` with `AttrPath` in **Section 2.3**:

- `AttrPath(Vec<String>)` handles arbitrary depth
- `AttrPath::parse("a.b.c")` returns 3 segments
- `builtins.hasAttrByPath` and `getAttrByPath` for Nix evaluation

##### 8. Worktree cleanup naming doesn't match current git code

**Issue**: `create_worktrees()` produces `worker-{i}` basenames (`git.rs:803`), but
`cleanup_worktrees()` filters for `nxv-worktree-` prefix (`git.rs:851`). Nothing is cleaned.

**Resolution**: Updated **Section 1.1** with:

- **Do NOT use `create_worktrees()`** - wrong naming convention
- Pool creates worktrees with basename `nxv-worktree-{pool_id}-{worker_id}`
- Full path: `/tmp/nxv-worktrees-abc123/nxv-worktree-abc123-0/`
- Git sees correct basename, cleanup works

#### Medium Priority (Round 2)

##### 9. Default backfill path still breaks for nested attrs

**Issue**: Head-mode backfill (default) at `backfill.rs:249-250` calls `extract_packages_for_attrs()`
which doesn't support dotted paths. Spec only mentioned historical mode in Phase 4.

**Resolution**: Added **Section 4.1 (Head-Mode Backfill for Nested Attrs)** with:

- Change `backfill.rs:249-250` to use `extract_packages_for_attr_paths()`
- Parse `attr_list` entries into `AttrPath` instances
- No worktree needed for head-mode

##### 10. File→attr map refresh rules remain top-level-only

**Issue**: `should_refresh_file_map()` at `mod.rs:1185-1197` only checks files like
`all-packages.nix`. Nested scope definitions live in other files, so map stays stale.

**Resolution**: Added **Section 2.5 (File Map Refresh for Nested Scopes)** with:

- Option A: Extend `TOP_LEVEL_FILES` with scope definition files
- Option B: Workers rebuild file→attr map per-commit
- Recommended: Option A for incremental, Option B for full index

##### 11. Worktree reuse conflicts with existing Drop semantics

**Issue**: `Worktree::drop()` at `git.rs:74-86` deletes the worktree. Pool expects handles
to return worktrees for reuse. Dropping handle would destroy worktree; pool Drop double-removes.

**Resolution**: Added **Section 1.2 (WorktreeHandle and Drop Semantics)** with:

- `PooledWorktree`: Internal struct, NO cleanup on Drop
- `WorktreeHandle<'pool>`: Borrows from pool, returns on drop (doesn't delete)
- `WorktreePool::Drop`: Explicitly removes all worktrees
- **Do NOT use existing `Worktree` struct** - conflicts with pool reuse

---

### Review Round 3 (Codex Validation)

This round addressed critical thread-safety and correctness issues that would have prevented
the implementation from working.

#### Architectural Questions Resolved

##### Q1: Worker ownership model

**Question**: Do you want each worker to own a single worktree for its entire lifetime
(simpler, avoids pool acquire/release) or a true pool with per-task checkout?

**Decision**: **Workers own worktrees for their lifetime.**

Rationale:

- `WorktreeHandle<'pool>` with borrow semantics cannot be moved into threads
- `git2::Repository` is `!Sync`, so `NixpkgsRepo` cannot be shared across threads
- Owned worktrees are simpler and avoid all lifetime issues
- Workers create worktree at startup, clean up via `Drop` on shutdown

Updated **Sections 1.1-1.2** with `OwnedWorktree` and `Worker` architecture.

##### Q2: Position extraction approach

**Question**: For nested change detection, is `meta.position`/`source_path`-based mapping
acceptable despite extra eval cost, or keep the faster-but-coarser `unsafeGetAttrPos`
mapping and accept misses?

**Decision**: **Use `meta.position`/`source_path`-based mapping.**

Rationale:

- `unsafeGetAttrPos` reports where attr is *defined* (e.g., `python-packages.nix`)
- `meta.position` reports where package is *implemented* (e.g., `numpy/default.nix`)
- Accurate mapping is essential for incremental indexing to work correctly
- Extra eval cost is amortized across parallel workers

Updated **Section 2.4** with `POSITIONS_FROM_META_NIX` approach.

##### Q3: File→attr map rebuild strategy

**Question**: In parallel mode, should we rebuild the file→attr map per commit (safe)
or precompute it sequentially and cache by commit hash?

**Decision**: **Rebuild per commit (safe).**

Rationale:

- Stateful `mapping_commit` tracking assumes sequential processing
- Parallel workers would race on shared map state
- Per-commit rebuild in each worker eliminates all race conditions
- Map building is part of package evaluation anyway (marginal additional cost)

Updated **Section 2.5** with per-commit rebuild strategy.

#### High Priority (Round 3)

##### 12. WorktreeHandle<'pool> cannot be moved into threads

**Issue**: The spec described `WorktreeHandle<'pool>` as borrowing from pool, but borrowed
references cannot be moved across thread boundaries. Additionally, `git2::Repository` is
`!Sync`, so `NixpkgsRepo` cannot be shared.

**Resolution**: Replaced pool-borrowing with worker-owned worktrees:

- **Section 1.1**: Changed `WorktreePool` to `WorktreeManager` concept
- **Section 1.2**: New `OwnedWorktree` and `Worker` types with move semantics
- Workers own their worktree for the entire session, no acquire/release

##### 13. unsafeGetAttrPos reports wrong file for nested packages

**Issue**: `unsafeGetAttrPos` returns the file where the attribute is defined in the scope
(e.g., `python-packages.nix:12345`), not where the package implementation lives
(e.g., `pkgs/development/python-modules/numpy/default.nix`).

**Resolution**: Updated **Section 2.4** to use `meta.position` instead:

- Use existing `getSourcePath` logic from `EXTRACT_NIX`
- Build file→attr map from package metadata, not position introspection
- Accurate mapping enables correct incremental updates for nested packages

##### 14. Parallel workers need per-commit file→attr map

**Issue**: The `mapping_commit` stateful logic at `mod.rs:840` (within `process_commits()`) assumes sequential processing.
Parallel workers processing out-of-order would use stale maps, causing incorrect updates.

**Resolution**: Updated **Section 2.5** with per-commit rebuild:

- Remove `should_refresh_file_map()` logic from main thread
- Each worker rebuilds `file_attr_map` for its commit in its worktree
- Workers send resolved `target_attr_paths` in `ExtractionResult`
- No shared map state, no races

#### Medium Priority (Round 3)

##### 15. Signal handler conflicts with existing ctrlc handlers

**Issue**: The spec proposed registering SIGTERM/SIGINT handlers in the pool, but
`main.rs:1119` and `main.rs:1217` already install global `ctrlc` handlers. Multiple
handlers conflict and signal-handler cleanup is unsafe.

**Resolution**: Added **Section 1.4 (Signal Handling)** with:

- Do NOT install new handlers in worktree manager or workers
- Use existing `Indexer::shutdown_flag()` (`mod.rs:574-576`)
- Workers check shutdown flag periodically
- `Drop` handles cleanup naturally

##### 16. AttrPath encoding and missing function reference

**Issue**: The spec referenced `extract_packages_for_scoped_attrs()` which doesn't exist.
AttrPath encoding was ambiguous (dotted strings vs list-of-segments).

**Resolution**: Updated **Section 2.3** with:

- Consistent use of `extract_packages_for_attr_paths()` throughout
- `AttrPath(Vec<String>)` for Rust representation
- JSON: `[["beam", "packages", "erlang_26"], ...]` (list of segment lists)
- Nix: Uses `builtins.getAttrByPath` directly with segment lists

#### Low Priority (Round 3)

##### 17. WorktreePool::Drop missing repo context

**Issue**: The spec example called `git worktree remove` without specifying the repository
context, which would fail if the current working directory isn't the repo.

**Resolution**: Updated **Section 1.1** code example to use:

```rust
Command::new("git")
    .arg("-C").arg(&self.repo_path)  // Critical: specify repo context
    .args(["worktree", "remove", "--force"])
    .arg(&self.path)
```
