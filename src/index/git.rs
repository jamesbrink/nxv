//! Git repository traversal for nixpkgs.

use crate::error::{NxvError, Result};
use chrono::{DateTime, TimeZone, Utc};
use git2::{Oid, Repository, Sort};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;
use tracing::{instrument, trace};

/// The earliest commit date we support for indexing.
/// Before this date, nixpkgs had a different structure that doesn't work
/// with modern Nix evaluation. This corresponds to early 2017 when
/// pname/version standardization began.
pub const MIN_INDEXABLE_DATE: &str = "2017-01-01";

/// The first commit in 2017, used as a default starting point.
#[allow(dead_code)]
pub const MIN_INDEXABLE_COMMIT: &str = "75ce71481842b0b9b1f81cd99cebf7aeba64243d";

/// Information about a git commit.
#[derive(Debug, Clone)]
pub struct CommitInfo {
    /// Full 40-character hash.
    pub hash: String,
    /// Commit timestamp.
    pub date: DateTime<Utc>,
    /// Short 7-character hash for display.
    pub short_hash: String,
}

impl CommitInfo {
    /// Create a CommitInfo from a git2::Commit.
    fn from_commit(commit: &git2::Commit) -> Self {
        let hash = commit.id().to_string();
        let short_hash = hash[..7].to_string();
        let timestamp = commit.time().seconds();
        let date = Utc.timestamp_opt(timestamp, 0).unwrap();

        Self {
            hash,
            date,
            short_hash,
        }
    }
}

/// Wrapper for nixpkgs git repository operations.
pub struct NixpkgsRepo {
    repo: Repository,
    #[allow(dead_code)]
    path: PathBuf,
}

/// A git worktree for parallel extraction.
pub struct Worktree {
    /// Path to the worktree directory.
    pub path: PathBuf,
    /// Path to the main repository for cleanup.
    repo_path: PathBuf,
    /// Whether this worktree should be cleaned up on drop.
    cleanup: bool,
}

impl Worktree {
    /// Create a new worktree handle.
    pub fn new(path: PathBuf, repo_path: PathBuf, cleanup: bool) -> Self {
        Self {
            path,
            repo_path,
            cleanup,
        }
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = Command::new("git")
                .current_dir(&self.repo_path)
                .args(["worktree", "remove", "--force"])
                .arg(&self.path)
                .output();

            // Remove the worktree directory
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// A managed worktree session for safe, fast commit traversal.
///
/// Creates a persistent worktree in a temp directory that can be quickly
/// checked out to different commits without modifying the main repository.
/// Auto-cleans up on drop (even on panic).
///
/// # Example
///
/// ```ignore
/// let session = WorktreeSession::new(&repo, "abc123")?;
/// for commit in commits {
///     session.checkout(&commit.hash)?;
///     // Work with files at session.path()
/// }
/// // Auto-cleanup on drop
/// ```
pub struct WorktreeSession {
    /// The underlying worktree.
    _worktree: Worktree,
    /// Path to the worktree directory.
    worktree_path: PathBuf,
    /// Temp directory that owns the worktree path (cleaned up on drop).
    _temp_dir: tempfile::TempDir,
}

impl WorktreeSession {
    /// Create a new worktree session in a temp directory.
    ///
    /// The worktree is initially checked out to `initial_commit`.
    /// Use `checkout()` to switch to different commits.
    pub fn new(repo: &NixpkgsRepo, initial_commit: &str) -> Result<Self> {
        let temp_dir = tempfile::tempdir().map_err(|e| {
            NxvError::Git(git2::Error::from_str(&format!(
                "Failed to create temp directory for worktree: {}",
                e
            )))
        })?;

        let worktree_path = temp_dir.path().join("nixpkgs");
        let worktree = repo.create_worktree(&worktree_path, initial_commit)?;

        Ok(Self {
            _worktree: worktree,
            worktree_path,
            _temp_dir: temp_dir,
        })
    }

    /// Checkout a different commit in this worktree.
    ///
    /// This is fast because it doesn't create a new worktree, just
    /// updates the existing one. Uses `git checkout -f` which forces
    /// the checkout and discards any local changes.
    #[instrument(skip(self))]
    pub fn checkout(&self, commit_hash: &str) -> Result<()> {
        let checkout_start = Instant::now();

        // Force checkout - no need for git clean since nix eval doesn't modify files
        let output = Command::new("git")
            .args(["checkout", "-f", commit_hash])
            .current_dir(&self.worktree_path)
            .output()
            .map_err(|e| {
                NxvError::Git(git2::Error::from_str(&format!(
                    "Failed to run git checkout: {}",
                    e
                )))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(NxvError::Git(git2::Error::from_str(&format!(
                "git checkout failed: {}",
                stderr.lines().take(3).collect::<Vec<_>>().join(" ")
            ))));
        }

        trace!(
            commit = %commit_hash,
            checkout_time_ms = checkout_start.elapsed().as_millis(),
            "Worktree checkout completed"
        );

        Ok(())
    }

    /// Get the path to the worktree directory.
    ///
    /// Use this path for file operations (e.g., running nix eval).
    pub fn path(&self) -> &Path {
        &self.worktree_path
    }
}

impl NixpkgsRepo {
    /// Open a nixpkgs repository at the given path.
    ///
    /// Validates that the path contains a valid nixpkgs repository
    /// by checking for the presence of the `pkgs/` directory.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let repo = Repository::open(&path)?;

        // Validate it's a nixpkgs repository
        let pkgs_dir = path.join("pkgs");
        if !pkgs_dir.exists() || !pkgs_dir.is_dir() {
            return Err(NxvError::NotNixpkgsRepo(format!(
                "Directory '{}' does not appear to be a nixpkgs repository (missing pkgs/ directory)",
                path.display()
            )));
        }

        Ok(Self { repo, path })
    }

    /// Prune orphaned worktrees from previous crashed indexer runs.
    ///
    /// This cleans up worktrees in `/tmp/.tmp*/` that were left behind when
    /// the indexer was killed or crashed without proper cleanup.
    ///
    /// Should be called at the start of indexing to reclaim disk space and
    /// prevent worktree name conflicts.
    pub fn prune_worktrees(&self) -> Result<()> {
        let output = Command::new("git")
            .current_dir(&self.path)
            .args(["worktree", "prune"])
            .output()
            .map_err(|e| {
                NxvError::Git(git2::Error::from_str(&format!(
                    "Failed to run git worktree prune: {}",
                    e
                )))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(
                stderr = %stderr,
                "git worktree prune failed (non-fatal)"
            );
        }

        Ok(())
    }

    /// Get all commits on the current branch in chronological order (oldest first).
    ///
    /// Uses first-parent traversal to avoid merge commit explosion.
    /// Note: This returns ALL commits including very old ones that may not
    /// be indexable. For indexing, use `get_indexable_commits` instead.
    #[allow(dead_code)]
    pub fn get_all_commits(&self) -> Result<Vec<CommitInfo>> {
        let head = self.repo.head()?;
        let head_commit = head.peel_to_commit()?;

        self.walk_commits_from(head_commit.id(), None, None)
    }

    /// Get commits that are indexable (from 2017 onwards).
    ///
    /// This filters out commits before MIN_INDEXABLE_DATE which have
    /// a different structure that doesn't work with modern Nix evaluation.
    #[allow(dead_code)]
    pub fn get_indexable_commits(&self) -> Result<Vec<CommitInfo>> {
        let min_date = chrono::NaiveDate::parse_from_str(MIN_INDEXABLE_DATE, "%Y-%m-%d")
            .expect("Invalid MIN_INDEXABLE_DATE format");
        let min_datetime = min_date
            .and_hms_opt(0, 0, 0)
            .expect("Invalid time")
            .and_utc();

        let head = self.repo.head()?;
        let head_commit = head.peel_to_commit()?;

        self.walk_commits_from(head_commit.id(), None, Some(min_datetime))
    }

    /// Get indexable commits that touched specific paths (newest first, then reversed).
    ///
    /// First tries with `--first-parent` to avoid processing duplicate commits from
    /// merge branches. If no commits are found (which can happen due to history
    /// restructuring in nixpkgs, e.g., the July-October 2020 gap), falls back to
    /// fetching all commits without `--first-parent`.
    pub fn get_indexable_commits_touching_paths(
        &self,
        paths: &[&str],
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<CommitInfo>> {
        // Try with --first-parent first for efficiency
        let commits = self.get_commits_touching_paths_inner(paths, since, until, true)?;

        // If no commits found with --first-parent, fall back to all commits.
        // This handles nixpkgs history gaps where the first-parent chain was broken
        // (e.g., July-October 2020 where --first-parent returns 0 commits).
        if commits.is_empty() {
            tracing::warn!(
                target: "nxv::index::git",
                "No commits found with --first-parent for range {:?} to {:?}, falling back to all commits",
                since, until
            );
            return self.get_commits_touching_paths_inner(paths, since, until, false);
        }

        Ok(commits)
    }

    /// Internal helper to get commits with optional --first-parent flag.
    fn get_commits_touching_paths_inner(
        &self,
        paths: &[&str],
        since: Option<&str>,
        until: Option<&str>,
        first_parent: bool,
    ) -> Result<Vec<CommitInfo>> {
        let since_arg = since.unwrap_or(MIN_INDEXABLE_DATE);
        let mut args = vec!["log", "--format=%H", "--since", since_arg];
        if first_parent {
            args.insert(1, "--first-parent");
        }
        if let Some(until) = until {
            args.push("--until");
            args.push(until);
        }
        args.push("--");

        let output = Command::new("git")
            .current_dir(&self.path)
            .args(&args)
            .args(paths)
            .output()?;

        if !output.status.success() {
            return Err(NxvError::Git(git2::Error::from_str(
                "Failed to list commits touching paths.",
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut commits = Vec::new();
        for line in stdout.lines() {
            let hash = line.trim();
            if hash.is_empty() {
                continue;
            }
            let oid = Oid::from_str(hash).map_err(|_| {
                NxvError::Git(git2::Error::from_str(&format!(
                    "Invalid commit hash: {}",
                    hash
                )))
            })?;
            let commit = self.repo.find_commit(oid)?;
            commits.push(CommitInfo::from_commit(&commit));
        }

        commits.reverse();
        Ok(commits)
    }

    /// Get commits since a specific hash that touched specific paths.
    pub fn get_commits_since_touching_paths(
        &self,
        since_hash: &str,
        paths: &[&str],
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<CommitInfo>> {
        let since_oid = Oid::from_str(since_hash).map_err(|_| {
            NxvError::Git(git2::Error::from_str(&format!(
                "Invalid commit hash: {}",
                since_hash
            )))
        })?;

        self.repo.find_commit(since_oid).map_err(|_| {
            NxvError::Git(git2::Error::from_str(&format!(
                "Invalid commit hash: {}",
                since_hash
            )))
        })?;

        let range = format!("{}..HEAD", since_hash);
        let mut args = vec!["log", "--first-parent", "--format=%H", &range];
        if let Some(since) = since {
            args.push("--since");
            args.push(since);
        }
        if let Some(until) = until {
            args.push("--until");
            args.push(until);
        }
        args.push("--");

        let output = Command::new("git")
            .current_dir(&self.path)
            .args(args)
            .args(paths)
            .output()?;

        if !output.status.success() {
            return Err(NxvError::Git(git2::Error::from_str(
                "Failed to list commits touching paths.",
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut commits = Vec::new();
        for line in stdout.lines() {
            let hash = line.trim();
            if hash.is_empty() {
                continue;
            }
            let oid = Oid::from_str(hash).map_err(|_| {
                NxvError::Git(git2::Error::from_str(&format!(
                    "Invalid commit hash: {}",
                    hash
                )))
            })?;
            let commit = self.repo.find_commit(oid)?;
            commits.push(CommitInfo::from_commit(&commit));
        }

        commits.reverse();
        Ok(commits)
    }

    /// Get changed paths for a commit (including rename sources and destinations).
    /// For merge commits, compares against first parent only to get actual PR changes.
    #[instrument(skip(self))]
    pub fn get_commit_changed_paths(&self, commit_hash: &str) -> Result<Vec<String>> {
        let diff_start = Instant::now();

        // For merge commits, compare against first parent (^1) to get just the PR changes.
        // For regular commits, this also works correctly.
        let output = Command::new("git")
            .current_dir(&self.path)
            .args([
                "diff",
                "--name-status",
                &format!("{}^1", commit_hash),
                commit_hash,
            ])
            .output()?;

        // If ^1 fails (e.g., initial commit), fall back to diff-tree
        let paths = if !output.status.success() {
            let fallback = Command::new("git")
                .current_dir(&self.path)
                .args(["diff-tree", "--name-status", "-r", commit_hash])
                .output()?;

            if !fallback.status.success() {
                return Err(NxvError::Git(git2::Error::from_str(
                    "Failed to list commit changes.",
                )));
            }

            Self::parse_diff_output(&String::from_utf8_lossy(&fallback.stdout))?
        } else {
            Self::parse_diff_output(&String::from_utf8_lossy(&output.stdout))?
        };

        trace!(
            commit = %commit_hash,
            changed_files = paths.len(),
            diff_time_ms = diff_start.elapsed().as_millis(),
            "Git diff completed"
        );

        Ok(paths)
    }

    /// Parse git diff --name-status output into a list of paths.
    fn parse_diff_output(output: &str) -> Result<Vec<String>> {
        let mut paths = Vec::new();
        for line in output.lines() {
            // Skip empty lines and commit hash lines (40 hex chars)
            if line.is_empty() || (line.len() == 40 && line.chars().all(|c| c.is_ascii_hexdigit()))
            {
                continue;
            }
            let mut parts = line.split('\t');
            let status = parts.next().unwrap_or_default();
            if status.starts_with('R') {
                // Rename: include both old and new paths
                if let Some(old_path) = parts.next() {
                    paths.push(old_path.to_string());
                }
                if let Some(new_path) = parts.next() {
                    paths.push(new_path.to_string());
                }
            } else if let Some(path) = parts.next() {
                paths.push(path.to_string());
            }
        }

        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    /// Get commits since a specific hash (exclusive, newest commits first, then reversed to chronological).
    ///
    /// Returns commits from `since_hash` (exclusive) to HEAD in chronological order.
    pub fn get_commits_since(&self, since_hash: &str) -> Result<Vec<CommitInfo>> {
        let head = self.repo.head()?;
        let head_commit = head.peel_to_commit()?;

        // Parse the since hash
        let since_oid = Oid::from_str(since_hash).map_err(|_| {
            NxvError::Git(git2::Error::from_str(&format!(
                "Invalid commit hash: {}",
                since_hash
            )))
        })?;

        // Verify the commit exists
        self.repo.find_commit(since_oid).map_err(|_| {
            NxvError::Git(git2::Error::from_str(&format!(
                "Commit not found: {}",
                since_hash
            )))
        })?;

        // If HEAD is the same as since, return empty
        if head_commit.id() == since_oid {
            return Ok(Vec::new());
        }

        self.walk_commits_from(head_commit.id(), Some(since_oid), None)
    }

    /// Walk commits from a starting point, optionally stopping before a given commit
    /// or filtering by minimum date.
    fn walk_commits_from(
        &self,
        from: Oid,
        stop_before: Option<Oid>,
        min_date: Option<DateTime<Utc>>,
    ) -> Result<Vec<CommitInfo>> {
        let mut revwalk = self.repo.revwalk()?;
        revwalk.push(from)?;

        // Use first-parent traversal to follow the main branch
        revwalk.simplify_first_parent()?;

        // Sort topologically (newest first by default after first-parent simplification)
        revwalk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;

        let mut commits = Vec::new();

        for oid_result in revwalk {
            let oid = oid_result?;

            // Stop if we've reached the stopping point
            if let Some(stop) = stop_before
                && oid == stop
            {
                break;
            }

            let commit = self.repo.find_commit(oid)?;
            let info = CommitInfo::from_commit(&commit);

            // Skip commits before the minimum date
            if let Some(min) = min_date
                && info.date < min
            {
                // Since we're walking newest-first, once we hit an old commit,
                // all remaining commits will be older, so we can stop
                break;
            }

            commits.push(info);
        }

        // Reverse to get chronological order (oldest first)
        commits.reverse();
        Ok(commits)
    }

    /// Switches the repository to a detached HEAD at the specified commit and updates the working directory.
    ///
    /// This will update the repository's HEAD to point to the given commit (detached) and modify the working tree;
    /// uncommitted changes may be overwritten.
    ///
    /// # Parameters
    ///
    /// * `hash` - Commit identifier (full or abbreviated) to check out.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the checkout succeeds, `Err(_)` if the operation fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// // Open repository and checkout a commit (example only).
    /// // let repo = NixpkgsRepo::open("/path/to/nixpkgs").unwrap();
    /// // repo.checkout_commit("a1b2c3d").unwrap();
    /// ```
    #[allow(dead_code)] // Kept for tests and future use; production uses WorktreeSession
    pub fn checkout_commit(&self, hash: &str) -> Result<()> {
        // Remove any stale index.lock file that might be left from a crashed process
        self.remove_index_lock();

        // Try libgit2 first (faster)
        if let Ok(()) = self.checkout_commit_libgit2(hash) {
            return Ok(());
        }

        // Fall back to command-line git (more robust with directory changes)
        self.checkout_commit_cli(hash)
    }

    /// Checks out the specified commit into a detached HEAD using libgit2 with aggressive force options.
    ///
    /// This operation sets HEAD to the given commit (detached) and updates the working tree using
    /// a forced checkout that removes untracked and ignored files and recreates missing files. It is
    /// intended to handle dirty working trees but can fail on complex directory changes where the
    /// libgit2 checkout cannot reconcile the working directory state.
    ///
    /// # Parameters
    ///
    /// - `hash`: Hexadecimal commit OID to check out.
    ///
    /// # Errors
    ///
    /// Returns an `Err` if the provided `hash` is not a valid OID or if any libgit2 operation fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use crate::index::git::NixpkgsRepo;
    /// # fn example(repo: &NixpkgsRepo) -> Result<(), Box<dyn std::error::Error>> {
    /// repo.checkout_commit_libgit2("0123456789abcdef0123456789abcdef01234567")?;
    /// # Ok(())
    /// # }
    /// ```
    #[allow(dead_code)]
    fn checkout_commit_libgit2(&self, hash: &str) -> Result<()> {
        let oid = Oid::from_str(hash).map_err(|_| {
            NxvError::Git(git2::Error::from_str(&format!(
                "Invalid commit hash: {}",
                hash
            )))
        })?;

        let commit = self.repo.find_commit(oid)?;
        let tree = commit.tree()?;

        // Use force checkout with aggressive options to handle dirty state
        let mut checkout_opts = git2::build::CheckoutBuilder::new();
        checkout_opts.force();
        checkout_opts.remove_untracked(true);
        checkout_opts.remove_ignored(true);
        checkout_opts.recreate_missing(true);

        self.repo
            .checkout_tree(tree.as_object(), Some(&mut checkout_opts))?;
        self.repo.set_head_detached(oid)?;

        Ok(())
    }

    /// Checkout the repository at the given commit using the system `git` CLI.
    ///
    /// This performs a workspace cleanup (`git clean -fdx`) followed by a forced
    /// checkout of the specified commit (`git checkout -f <hash>`). Returns an
    /// error if either command fails; failure details include up to three lines of
    /// stderr from the failing `git` invocation.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use your_crate::index::git::NixpkgsRepo;
    /// // Open an existing repository and checkout a commit by hash.
    /// let repo = NixpkgsRepo::open("/path/to/nixpkgs").unwrap();
    /// repo.checkout_commit_cli("0123456789abcdef0123456789abcdef01234567").unwrap();
    /// ```
    #[allow(dead_code)]
    fn checkout_commit_cli(&self, hash: &str) -> Result<()> {
        let repo_path = self.path();

        // Run git clean first to remove any untracked files/directories
        let clean_output = Command::new("git")
            .args(["clean", "-fdx"])
            .current_dir(repo_path)
            .output()
            .map_err(|e| {
                NxvError::Git(git2::Error::from_str(&format!(
                    "Failed to run git clean: {}",
                    e
                )))
            })?;

        if !clean_output.status.success() {
            let stderr = String::from_utf8_lossy(&clean_output.stderr);
            let stdout = String::from_utf8_lossy(&clean_output.stdout);
            return Err(NxvError::Git(git2::Error::from_str(&format!(
                "git clean failed with exit code {}: {}{}",
                clean_output
                    .status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                stderr.trim(),
                if !stdout.is_empty() {
                    format!(" (stdout: {})", stdout.trim())
                } else {
                    String::new()
                }
            ))));
        }

        // Then checkout the commit
        let output = Command::new("git")
            .args(["checkout", "-f", hash])
            .current_dir(repo_path)
            .output()
            .map_err(|e| {
                NxvError::Git(git2::Error::from_str(&format!(
                    "Failed to run git checkout: {}",
                    e
                )))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(NxvError::Git(git2::Error::from_str(&format!(
                "git checkout failed: {}",
                stderr.lines().take(3).collect::<Vec<_>>().join(" ")
            ))));
        }

        Ok(())
    }

    /// Remove stale index.lock file if it exists.
    fn remove_index_lock(&self) {
        let git_dir = self.repo.path();
        let lock_file = git_dir.join("index.lock");
        if lock_file.exists() {
            let _ = std::fs::remove_file(&lock_file);
        }
    }

    /// Obtain the full commit hash that HEAD points to.
    ///
    /// # Returns
    ///
    /// `Ok(String)` containing the 40-character commit hash of HEAD.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// let repo = NixpkgsRepo::open("/path/to/nixpkgs").unwrap();
    /// let head = repo.head_commit().unwrap();
    /// assert_eq!(head.len(), 40);
    /// ```
    #[allow(dead_code)]
    pub fn head_commit(&self) -> Result<String> {
        let head = self.repo.head()?;
        let commit = head.peel_to_commit()?;
        Ok(commit.id().to_string())
    }

    /// Gets the current HEAD reference name or the detached HEAD commit hash.
    ///
    /// If HEAD points to a branch, returns the full reference name (for example, `refs/heads/master`).
    /// If HEAD is detached, returns the full 40-character commit hash.
    ///
    /// # Examples
    ///
    /// ```
    /// let repo = NixpkgsRepo::open("/path/to/repo").unwrap();
    /// let head_ref = repo.head_ref().unwrap();
    /// // either a ref like "refs/heads/main" or a 40-char commit hash
    /// assert!(head_ref.starts_with("refs/heads/") || head_ref.len() == 40);
    /// ```
    #[allow(dead_code)] // Kept for potential future use; production uses WorktreeSession
    pub fn head_ref(&self) -> Result<String> {
        let head = self.repo.head()?;
        if head.is_branch() {
            // Return the full reference name
            Ok(head
                .name()
                .ok_or_else(|| NxvError::Git(git2::Error::from_str("HEAD has no name")))?
                .to_string())
        } else {
            // Detached HEAD - return commit hash
            let commit = head.peel_to_commit()?;
            Ok(commit.id().to_string())
        }
    }

    /// Restore the repository's HEAD to a given branch reference or commit.
    ///
    /// If `ref_name` starts with `"refs/"` the corresponding branch reference is checked out
    /// and HEAD is updated to that ref. Otherwise `ref_name` is treated as a commit hash and
    /// the repository is checked out in detached HEAD at that commit.
    ///
    /// # Parameters
    ///
    /// - `ref_name`: a branch reference name (e.g., `"refs/heads/main"`) or a commit hash (e.g., `"a1b2c3..."`).
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or an error if any git operation (lookup, checkout, or setting HEAD) fails.
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::path::Path;
    /// # fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let repo = NixpkgsRepo::open(Path::new("/path/to/nixpkgs"))?;
    /// // Restore to branch:
    /// repo.restore_ref("refs/heads/main")?;
    /// // Or restore to a specific commit:
    /// repo.restore_ref("0123456789abcdef0123456789abcdef01234567")?;
    /// # Ok(())
    /// # }
    /// ```
    #[allow(dead_code)] // Kept for potential future use; production uses WorktreeSession
    pub fn restore_ref(&self, ref_name: &str) -> Result<()> {
        if ref_name.starts_with("refs/") {
            // It's a branch reference - checkout the branch
            let reference = self.repo.find_reference(ref_name)?;
            let commit = reference.peel_to_commit()?;
            let tree = commit.tree()?;

            let mut checkout_opts = git2::build::CheckoutBuilder::new();
            checkout_opts.force();

            self.repo
                .checkout_tree(tree.as_object(), Some(&mut checkout_opts))?;
            self.repo.set_head(ref_name)?;
        } else {
            // It's a commit hash - checkout detached
            self.checkout_commit(ref_name)?;
        }

        Ok(())
    }

    /// Return the number of commits reachable from HEAD following the first-parent chain.
    ///
    /// Counts commits reachable from HEAD using first-parent simplification (mainline),
    /// suitable for progress reporting.
    ///
    /// # Returns
    ///
    /// The total number of commits reachable from HEAD.
    ///
    /// # Examples
    ///
    /// ```
    /// // Open a repository and count commits reachable from HEAD.
    /// let repo = NixpkgsRepo::open("/path/to/repo").unwrap();
    /// let total = repo.count_commits().unwrap();
    /// assert!(total >= 0);
    /// ```
    #[allow(dead_code)]
    pub fn count_commits(&self) -> Result<usize> {
        let head = self.repo.head()?;
        let head_commit = head.peel_to_commit()?;

        let mut revwalk = self.repo.revwalk()?;
        revwalk.push(head_commit.id())?;
        revwalk.simplify_first_parent()?;

        Ok(revwalk.count())
    }

    /// Count commits since a specific hash.
    #[allow(dead_code)]
    pub fn count_commits_since(&self, since_hash: &str) -> Result<usize> {
        let commits = self.get_commits_since(since_hash)?;
        Ok(commits.len())
    }

    /// Accesses the repository's filesystem path.
    ///
    /// Returns a reference to the repository root directory as a `Path`.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// // given a `repo: NixpkgsRepo`
    /// let repo_path = repo.path();
    /// println!("repo path: {}", repo_path.display());
    /// ```
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Determines whether one commit is an ancestor of another.
    ///
    /// # Returns
    ///
    /// `true` if the commit identified by `ancestor_hash` is reachable from the commit identified by `descendant_hash`, `false` otherwise.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// let repo = NixpkgsRepo::open("/path/to/nixpkgs").unwrap();
    /// let ancestor = "0123456789abcdef0123456789abcdef01234567";
    /// let descendant = "89abcdef0123456789abcdef0123456789abcdef";
    /// let is_anc = repo.is_ancestor(ancestor, descendant).unwrap();
    /// ```
    pub fn is_ancestor(&self, ancestor_hash: &str, descendant_hash: &str) -> Result<bool> {
        let output = Command::new("git")
            .current_dir(&self.path)
            .args([
                "merge-base",
                "--is-ancestor",
                ancestor_hash,
                descendant_hash,
            ])
            .output()?;

        // Exit code 0 means ancestor_hash IS an ancestor of descendant_hash
        // Exit code 1 means it is NOT an ancestor
        // Other exit codes indicate an error
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(NxvError::Git(git2::Error::from_str(&format!(
                    "Failed to check ancestry: {}",
                    stderr.trim()
                ))))
            }
        }
    }

    /// Create a new worktree at the specified path, checked out to a specific commit.
    ///
    /// Worktrees allow parallel checkouts of different commits without modifying
    /// the main repository's working directory.
    pub fn create_worktree(&self, worktree_path: &Path, commit_hash: &str) -> Result<Worktree> {
        let output = Command::new("git")
            .current_dir(&self.path)
            .args(["worktree", "add", "--detach"])
            .arg(worktree_path)
            .arg(commit_hash)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(NxvError::Git(git2::Error::from_str(
                stderr
                    .lines()
                    .take(3)
                    .collect::<Vec<_>>()
                    .join("\n")
                    .as_str(),
            )));
        }

        Ok(Worktree::new(
            worktree_path.to_path_buf(),
            self.path.clone(),
            true,
        ))
    }

    /// Create multiple worktrees for parallel processing.
    ///
    /// Returns a vector of worktrees, each checked out to a different commit.
    #[allow(dead_code)]
    pub fn create_worktrees(&self, base_path: &Path, commits: &[&str]) -> Result<Vec<Worktree>> {
        let mut worktrees = Vec::with_capacity(commits.len());

        for (i, commit_hash) in commits.iter().enumerate() {
            let worktree_path = base_path.join(format!("worker-{}", i));
            worktrees.push(self.create_worktree(&worktree_path, commit_hash)?);
        }

        Ok(worktrees)
    }

    /// Remove a worktree by name.
    #[allow(dead_code)]
    pub fn remove_worktree(&self, name: &str) -> Result<()> {
        // Find and prune the worktree
        if let Ok(worktree) = self.repo.find_worktree(name) {
            worktree.prune(Some(
                git2::WorktreePruneOptions::new()
                    .working_tree(true)
                    .valid(true)
                    .locked(false),
            ))?;
        }

        // Also try to delete the branch
        let branch_ref = format!("refs/heads/{}", name);
        if let Ok(mut reference) = self.repo.find_reference(&branch_ref) {
            let _ = reference.delete();
        }

        Ok(())
    }

    /// Removes all worktrees whose names start with "nxv-worktree-" from the repository.
    ///
    /// Returns an error if listing repository worktrees fails. Failures while removing
    /// individual worktrees are ignored and do not stop the cleanup of other worktrees.
    ///
    /// # Examples
    ///
    /// ```
    /// let repo = NixpkgsRepo::open("/path/to/nixpkgs").unwrap();
    /// repo.cleanup_worktrees().unwrap();
    /// ```
    #[allow(dead_code)]
    pub fn cleanup_worktrees(&self) -> Result<()> {
        // List all worktrees and remove ones starting with "nxv-worktree-"
        let worktrees: Vec<String> = self
            .repo
            .worktrees()?
            .iter()
            .filter_map(|s| s.map(String::from))
            .filter(|name| name.starts_with("nxv-worktree-"))
            .collect();

        for name in worktrees {
            let _ = self.remove_worktree(&name);
        }

        Ok(())
    }

    /// Fetches updates from the `origin` remote.
    ///
    /// Attempts to run `git fetch origin` in the repository working directory.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the fetch completes successfully; `Err(NxvError::Git)` if the git command fails to run or returns a non-zero exit status. The error will include up to three lines of stderr from the git process.
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::env::temp_dir;
    /// # // assume `repo` is a prepared `NixpkgsRepo` opened for a test
    /// # let repo = { let p = temp_dir(); crate::index::git::NixpkgsRepo::open(&p).unwrap() };
    /// repo.fetch_origin().unwrap();
    /// ```
    pub fn fetch_origin(&self) -> Result<()> {
        let output = Command::new("git")
            .args(["fetch", "origin"])
            .current_dir(&self.path)
            .output()
            .map_err(|e| {
                NxvError::Git(git2::Error::from_str(&format!(
                    "Failed to run git fetch: {}",
                    e
                )))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(NxvError::Git(git2::Error::from_str(&format!(
                "git fetch failed: {}",
                stderr.lines().take(3).collect::<Vec<_>>().join(" ")
            ))));
        }

        Ok(())
    }

    /// Reset the repository to a clean state by hard-resetting to `target` (default `origin/nixpkgs-unstable`)
    /// and removing all untracked files and directories.
    ///
    /// If `target` is `None`, `origin/nixpkgs-unstable` is used.
    ///
    /// # Errors
    /// Returns an `Err` if the underlying git commands (`reset` or `clean`) fail or cannot be executed.
    ///
    /// # Examples
    ///
    /// ```
    /// // Reset to origin/nixpkgs-unstable
    /// repo.reset_hard(None).unwrap();
    ///
    /// // Reset to a specific ref
    /// repo.reset_hard(Some("refs/heads/main")).unwrap();
    /// ```
    pub fn reset_hard(&self, target: Option<&str>) -> Result<()> {
        // Remove any stale lock files
        self.remove_index_lock();

        let target_ref = target.unwrap_or("origin/nixpkgs-unstable");

        // First, try to abort any in-progress operations
        let _ = Command::new("git")
            .args(["merge", "--abort"])
            .current_dir(&self.path)
            .output();
        let _ = Command::new("git")
            .args(["rebase", "--abort"])
            .current_dir(&self.path)
            .output();
        let _ = Command::new("git")
            .args(["cherry-pick", "--abort"])
            .current_dir(&self.path)
            .output();

        // Reset to target ref
        let output = Command::new("git")
            .args(["reset", "--hard", target_ref])
            .current_dir(&self.path)
            .output()
            .map_err(|e| {
                NxvError::Git(git2::Error::from_str(&format!(
                    "Failed to run git reset: {}",
                    e
                )))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(NxvError::Git(git2::Error::from_str(&format!(
                "git reset failed: {}",
                stderr.lines().take(3).collect::<Vec<_>>().join(" ")
            ))));
        }

        // Clean untracked files and directories
        let output = Command::new("git")
            .args(["clean", "-fdx"])
            .current_dir(&self.path)
            .output()
            .map_err(|e| {
                NxvError::Git(git2::Error::from_str(&format!(
                    "Failed to run git clean: {}",
                    e
                )))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(NxvError::Git(git2::Error::from_str(&format!(
                "git clean failed: {}",
                stderr.lines().take(3).collect::<Vec<_>>().join(" ")
            ))));
        }

        Ok(())
    }

    /// Get the diff content for a specific file at a given commit.
    ///
    /// This returns the actual diff text (with +/- lines) for the specified file,
    /// which can be parsed to extract affected attribute names from infrastructure files.
    ///
    /// # Arguments
    /// * `commit_hash` - The commit to get the diff for
    /// * `file_path` - The path to the file to get the diff for (e.g., "pkgs/top-level/all-packages.nix")
    ///
    /// # Returns
    /// The diff content as a string, or an error if the operation fails.
    #[instrument(skip(self))]
    pub fn get_file_diff(&self, commit_hash: &str, file_path: &str) -> Result<String> {
        // Compare against first parent for merge commits
        let output = Command::new("git")
            .current_dir(&self.path)
            .args([
                "diff",
                "-U0", // Unified diff with 0 context lines (just the changes)
                &format!("{}^1", commit_hash),
                commit_hash,
                "--",
                file_path,
            ])
            .output()?;

        // If ^1 fails (e.g., initial commit), fall back to showing the file
        if !output.status.success() {
            let fallback = Command::new("git")
                .current_dir(&self.path)
                .args(["show", &format!("{}:{}", commit_hash, file_path)])
                .output()?;

            if fallback.status.success() {
                // Return the whole file content prefixed with + (treat as additions)
                let content = String::from_utf8_lossy(&fallback.stdout);
                return Ok(content
                    .lines()
                    .map(|l| format!("+{}", l))
                    .collect::<Vec<_>>()
                    .join("\n"));
            }

            return Ok(String::new());
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    /// Create a test git repository with known commits.
    fn create_test_repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();

        // Initialize git repo
        Command::new("git")
            .args(["init"])
            .current_dir(&path)
            .output()
            .expect("Failed to init git repo");

        // Configure git user for commits
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&path)
            .output()
            .expect("Failed to configure git email");

        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&path)
            .output()
            .expect("Failed to configure git name");

        // Create pkgs directory to make it look like nixpkgs
        std::fs::create_dir(path.join("pkgs")).unwrap();

        // Create initial commit
        std::fs::write(path.join("file1.txt"), "content1").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .expect("Failed to add files");
        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(&path)
            .output()
            .expect("Failed to create commit");

        // Create second commit
        std::fs::write(path.join("file2.txt"), "content2").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .expect("Failed to add files");
        Command::new("git")
            .args(["commit", "-m", "Second commit"])
            .current_dir(&path)
            .output()
            .expect("Failed to create commit");

        // Create third commit
        std::fs::write(path.join("file3.txt"), "content3").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .expect("Failed to add files");
        Command::new("git")
            .args(["commit", "-m", "Third commit"])
            .current_dir(&path)
            .output()
            .expect("Failed to create commit");

        (dir, path)
    }

    #[test]
    fn test_open_valid_repo() {
        let (_dir, path) = create_test_repo();
        let repo = NixpkgsRepo::open(&path);
        assert!(repo.is_ok());
    }

    #[test]
    fn test_open_non_git_directory() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join("pkgs")).unwrap();
        let result = NixpkgsRepo::open(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_open_non_nixpkgs_repo() {
        let dir = tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .expect("Failed to init git repo");

        let result = NixpkgsRepo::open(dir.path());
        assert!(matches!(result, Err(NxvError::NotNixpkgsRepo(_))));
    }

    #[test]
    fn test_get_all_commits_chronological_order() {
        let (_dir, path) = create_test_repo();
        let repo = NixpkgsRepo::open(&path).unwrap();

        let commits = repo.get_all_commits().unwrap();
        assert_eq!(commits.len(), 3);

        // Verify chronological order (oldest first)
        // Note: Since commits may have same timestamp (created in quick succession),
        // we check that order is at least non-descending
        assert!(commits[0].date <= commits[1].date);
        assert!(commits[1].date <= commits[2].date);

        // Also verify that the first commit is indeed the first in git log
        // by checking that the last commit is HEAD
        let head = repo.head_commit().unwrap();
        assert_eq!(commits[2].hash, head);
    }

    #[test]
    fn test_get_commits_since_known_hash() {
        let (_dir, path) = create_test_repo();
        let repo = NixpkgsRepo::open(&path).unwrap();

        let all_commits = repo.get_all_commits().unwrap();
        let first_commit_hash = &all_commits[0].hash;

        // Get commits since first commit should return 2 (second and third)
        let commits_since = repo.get_commits_since(first_commit_hash).unwrap();
        assert_eq!(commits_since.len(), 2);

        // Verify it's the second and third commits
        assert_eq!(commits_since[0].hash, all_commits[1].hash);
        assert_eq!(commits_since[1].hash, all_commits[2].hash);
    }

    #[test]
    fn test_get_commits_since_head() {
        let (_dir, path) = create_test_repo();
        let repo = NixpkgsRepo::open(&path).unwrap();

        let head = repo.head_commit().unwrap();
        let commits = repo.get_commits_since(&head).unwrap();
        assert!(commits.is_empty());
    }

    #[test]
    fn test_get_commits_since_unknown_hash() {
        let (_dir, path) = create_test_repo();
        let repo = NixpkgsRepo::open(&path).unwrap();

        let result = repo.get_commits_since("0000000000000000000000000000000000000000");
        assert!(result.is_err());
    }

    #[test]
    fn test_count_commits() {
        let (_dir, path) = create_test_repo();
        let repo = NixpkgsRepo::open(&path).unwrap();

        let count = repo.count_commits().unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn test_head_commit() {
        let (_dir, path) = create_test_repo();
        let repo = NixpkgsRepo::open(&path).unwrap();

        let head = repo.head_commit().unwrap();
        assert_eq!(head.len(), 40); // SHA-1 hash length
    }

    #[test]
    #[ignore] // This test modifies the working directory
    fn test_checkout_commit() {
        let (_dir, path) = create_test_repo();
        let repo = NixpkgsRepo::open(&path).unwrap();

        let all_commits = repo.get_all_commits().unwrap();
        let first_commit = &all_commits[0].hash;

        // Checkout first commit
        repo.checkout_commit(first_commit).unwrap();

        // file2.txt and file3.txt should not exist
        assert!(!path.join("file2.txt").exists());
        assert!(!path.join("file3.txt").exists());
    }

    #[test]
    fn test_open_real_nixpkgs_submodule() {
        // This test uses the real nixpkgs submodule if it exists
        let nixpkgs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("nixpkgs");

        // Check if nixpkgs submodule is properly initialized (has .git file/dir and pkgs/)
        let has_git = nixpkgs_path.join(".git").exists();
        let has_pkgs = nixpkgs_path.join("pkgs").exists();

        if !has_git || !has_pkgs {
            // Skip if nixpkgs submodule isn't properly initialized
            eprintln!(
                "Skipping: nixpkgs submodule not properly initialized at {:?} (has_git={}, has_pkgs={})",
                nixpkgs_path, has_git, has_pkgs
            );
            return;
        }

        let repo = NixpkgsRepo::open(&nixpkgs_path).unwrap();
        let head = repo.head_commit().unwrap();
        assert_eq!(head.len(), 40);

        // Should be able to count commits (just verify it doesn't error)
        let count = repo.count_commits().unwrap();
        assert!(count > 0);
    }

    #[test]
    fn test_is_ancestor() {
        let (_dir, path) = create_test_repo();
        let repo = NixpkgsRepo::open(&path).unwrap();

        let commits = repo.get_all_commits().unwrap();
        assert_eq!(commits.len(), 3);

        let first = &commits[0].hash;
        let second = &commits[1].hash;
        let third = &commits[2].hash;

        // First commit is an ancestor of third
        assert!(repo.is_ancestor(first, third).unwrap());

        // First commit is an ancestor of second
        assert!(repo.is_ancestor(first, second).unwrap());

        // Third commit is NOT an ancestor of first (it's newer)
        assert!(!repo.is_ancestor(third, first).unwrap());

        // A commit is an ancestor of itself
        assert!(repo.is_ancestor(first, first).unwrap());
    }

    #[test]
    fn test_get_commit_changed_paths_includes_rename() {
        let (_dir, path) = create_test_repo();
        let repo = NixpkgsRepo::open(&path).unwrap();

        std::fs::write(path.join("file.txt"), "one").unwrap();
        Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(&path)
            .output()
            .expect("Failed to add file");
        Command::new("git")
            .args(["commit", "-m", "Add file"])
            .current_dir(&path)
            .output()
            .expect("Failed to commit file");

        Command::new("git")
            .args(["mv", "file.txt", "file-renamed.txt"])
            .current_dir(&path)
            .output()
            .expect("Failed to rename file");
        Command::new("git")
            .args(["commit", "-am", "Rename file"])
            .current_dir(&path)
            .output()
            .expect("Failed to commit rename");

        let head = repo.head_commit().unwrap();
        let changed = repo.get_commit_changed_paths(&head).unwrap();
        assert!(changed.contains(&"file.txt".to_string()));
        assert!(changed.contains(&"file-renamed.txt".to_string()));
    }

    #[test]
    fn test_get_indexable_commits_touching_paths_fallback() {
        // This test verifies the fallback from --first-parent to all commits
        // when --first-parent returns no commits.
        //
        // This can happen in nixpkgs due to history restructuring.
        // Example: July-October 2020 where --first-parent returns 0 commits
        // but the full history has thousands of commits.

        let nixpkgs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("nixpkgs");

        // Skip if nixpkgs submodule isn't properly initialized
        if !nixpkgs_path.join(".git").exists() || !nixpkgs_path.join("pkgs").exists() {
            eprintln!("Skipping: nixpkgs submodule not initialized");
            return;
        }

        let repo = NixpkgsRepo::open(&nixpkgs_path).unwrap();

        // July-October 2020 is known to have 0 commits with --first-parent
        // but thousands of commits without it. Test that we get commits.
        let commits = repo
            .get_indexable_commits_touching_paths(
                &["pkgs"],
                Some("2020-07-01"),
                Some("2020-10-01"),
            )
            .unwrap();

        // Should have commits thanks to the fallback
        assert!(
            !commits.is_empty(),
            "Expected commits in 2020-07 to 2020-10 range (fallback should have activated)"
        );

        // Should have a significant number of commits (the gap period has ~11k commits)
        assert!(
            commits.len() > 1000,
            "Expected >1000 commits in gap period, got {}",
            commits.len()
        );
    }
}
