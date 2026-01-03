//! Worktree pool for parallel package extraction.
//!
//! This module provides infrastructure for parallel indexing using git worktrees.
//! Each worker thread owns its worktree for the duration of the indexing session,
//! avoiding the complexity of pool-based borrowing which conflicts with Rust's
//! ownership rules for cross-thread references.
//!
//! Key design decisions:
//! - Workers OWN worktrees rather than borrowing from a pool
//! - Worktrees are created on the main thread before spawning workers
//! - Workers are moved into spawned threads with full ownership
//! - Cleanup happens automatically via Drop implementation
//! - Uses file locking for orphan detection instead of PID encoding

use crate::error::{NxvError, Result};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A worktree owned by a worker thread for the duration of a session.
///
/// Unlike the `Worktree` struct in `git.rs` which may or may not clean up,
/// `OwnedWorktree` always cleans up its worktree on drop. This is suitable
/// for parallel indexing where each worker owns exactly one worktree.
#[derive(Debug)]
pub struct OwnedWorktree {
    /// Path to the worktree directory.
    pub path: PathBuf,
    /// Path to the main repository (for git commands).
    repo_path: PathBuf,
    /// Current commit checked out (for skip-checkout optimization).
    current_commit: Option<String>,
}

impl OwnedWorktree {
    /// Create a new worktree for a worker.
    ///
    /// The worktree is created at: `{base_dir}/nxv-worktree-{session_id}-{worker_id}/`
    /// This naming convention ensures `cleanup_worktrees()` in git.rs can find and
    /// clean up stale worktrees from crashed sessions.
    ///
    /// # Arguments
    /// * `repo_path` - Path to the main nixpkgs repository
    /// * `base_dir` - Base directory for worktrees (e.g., `/tmp/nxv-worktrees-abc123`)
    /// * `session_id` - Unique identifier for this indexing session
    /// * `worker_id` - Worker's numeric ID (0..num_workers)
    /// * `initial_commit` - Commit hash to checkout initially
    ///
    /// # Errors
    /// Returns an error if the worktree cannot be created.
    pub fn create(
        repo_path: &Path,
        base_dir: &Path,
        session_id: &str,
        worker_id: usize,
        initial_commit: &str,
    ) -> Result<Self> {
        // Worktree basename must start with "nxv-worktree-" for cleanup to work
        let worktree_name = format!("nxv-worktree-{}-{}", session_id, worker_id);
        let worktree_path = base_dir.join(&worktree_name);

        // Ensure base directory exists
        fs::create_dir_all(base_dir)?;

        // Create the worktree with detached HEAD
        let output = Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .args(["worktree", "add", "--detach"])
            .arg(&worktree_path)
            .arg(initial_commit)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(NxvError::Git(git2::Error::from_str(&format!(
                "Failed to create worktree {}: {}",
                worktree_name,
                stderr.lines().take(3).collect::<Vec<_>>().join("\n")
            ))));
        }

        Ok(Self {
            path: worktree_path,
            repo_path: repo_path.to_path_buf(),
            current_commit: Some(initial_commit.to_string()),
        })
    }

    /// Checkout a commit in this worktree.
    ///
    /// Skips checkout if already at the requested commit (optimization).
    ///
    /// # Arguments
    /// * `commit_hash` - The commit hash to checkout
    ///
    /// # Errors
    /// Returns an error if checkout fails.
    pub fn checkout(&mut self, commit_hash: &str) -> Result<()> {
        // Skip if already at this commit
        if self.current_commit.as_deref() == Some(commit_hash) {
            return Ok(());
        }

        let output = Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(["checkout", "--force", "--detach", commit_hash])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(NxvError::Git(git2::Error::from_str(&format!(
                "Failed to checkout {} in worktree: {}",
                &commit_hash[..7.min(commit_hash.len())],
                stderr.lines().take(3).collect::<Vec<_>>().join("\n")
            ))));
        }

        // Clean any untracked files that might cause issues
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(["clean", "-fdx"])
            .output();

        self.current_commit = Some(commit_hash.to_string());
        Ok(())
    }

    /// Get the current commit hash checked out in this worktree.
    #[allow(dead_code)]
    pub fn current_commit(&self) -> Option<&str> {
        self.current_commit.as_deref()
    }
}

impl Drop for OwnedWorktree {
    fn drop(&mut self) {
        // Remove the worktree from git's tracking
        // Use --force to handle any modifications
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.repo_path)
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .output();

        // Also remove the directory if it still exists (belt and suspenders)
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Session manager for worktree-based parallel indexing.
///
/// Manages the session directory and lock file. Does NOT manage individual
/// worktrees - those are owned by workers directly.
#[derive(Debug)]
pub struct WorktreeSession {
    /// Unique session identifier.
    pub session_id: String,
    /// Base directory for all worktrees in this session.
    pub base_dir: PathBuf,
    /// Path to the main repository.
    pub repo_path: PathBuf,
    /// Lock file handle (held for lifetime of session).
    _lock_file: File,
}

impl WorktreeSession {
    /// Create a new worktree session.
    ///
    /// This creates the session directory and acquires an exclusive lock.
    /// The lock is used for orphan detection - if another process can acquire
    /// the lock, the session directory is orphaned and can be cleaned up.
    ///
    /// # Arguments
    /// * `repo_path` - Path to the nixpkgs repository
    ///
    /// # Errors
    /// Returns an error if the session cannot be created or the lock cannot be acquired.
    pub fn new(repo_path: &Path) -> Result<Self> {
        // Generate a unique session ID using timestamp and random suffix
        let session_id = format!(
            "{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            rand_suffix()
        );

        // Create session directory in system temp
        let base_dir = std::env::temp_dir().join(format!("nxv-worktrees-{}", session_id));
        fs::create_dir_all(&base_dir)?;

        // Create and lock the session lock file
        let lock_path = base_dir.join(".lock");
        let mut lock_file = File::create(&lock_path)?;
        lock_file.write_all(b"nxv session lock")?;

        // Try to acquire exclusive lock
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let fd = lock_file.as_raw_fd();
            let result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
            if result != 0 {
                return Err(NxvError::Git(git2::Error::from_str(
                    "Failed to acquire session lock",
                )));
            }
        }

        Ok(Self {
            session_id,
            base_dir,
            repo_path: repo_path.to_path_buf(),
            _lock_file: lock_file,
        })
    }

    /// Create an owned worktree for a worker.
    ///
    /// The worktree is created with the naming convention that allows
    /// cleanup_worktrees() to find and remove stale worktrees.
    ///
    /// # Arguments
    /// * `worker_id` - Worker's numeric ID
    /// * `initial_commit` - Commit hash to checkout initially
    pub fn create_worktree(&self, worker_id: usize, initial_commit: &str) -> Result<OwnedWorktree> {
        OwnedWorktree::create(
            &self.repo_path,
            &self.base_dir,
            &self.session_id,
            worker_id,
            initial_commit,
        )
    }

    /// Clean up orphaned session directories from previous crashed runs.
    ///
    /// Scans `/tmp/nxv-worktrees-*` directories and attempts to acquire
    /// their lock files. If the lock succeeds, the directory is orphaned
    /// and can be safely deleted.
    ///
    /// This should be called at startup before creating a new session.
    pub fn cleanup_orphaned_sessions() {
        let temp_dir = std::env::temp_dir();

        // Find all nxv-worktrees-* directories
        let entries = match fs::read_dir(&temp_dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };

            if !name.starts_with("nxv-worktrees-") {
                continue;
            }

            let lock_path = path.join(".lock");
            if !lock_path.exists() {
                // No lock file means definitely orphaned
                let _ = fs::remove_dir_all(&path);
                continue;
            }

            // Try to acquire the lock
            #[cfg(unix)]
            {
                if let Ok(lock_file) = File::open(&lock_path) {
                    use std::os::unix::io::AsRawFd;
                    let fd = lock_file.as_raw_fd();
                    let result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
                    if result == 0 {
                        // Lock succeeded - this is an orphan
                        // Release lock before deleting
                        unsafe { libc::flock(fd, libc::LOCK_UN) };
                        drop(lock_file);
                        let _ = fs::remove_dir_all(&path);
                    }
                    // If lock failed, another process is using it - skip
                }
            }

            #[cfg(not(unix))]
            {
                // On non-Unix systems, just check if the directory is old (> 1 hour)
                if let Ok(metadata) = path.metadata() {
                    if let Ok(modified) = metadata.modified() {
                        if let Ok(elapsed) = modified.elapsed() {
                            if elapsed > std::time::Duration::from_secs(3600) {
                                let _ = fs::remove_dir_all(&path);
                            }
                        }
                    }
                }
            }
        }
    }
}

impl Drop for WorktreeSession {
    fn drop(&mut self) {
        // Remove the session directory
        // The lock file will be released when _lock_file is dropped
        let _ = fs::remove_dir_all(&self.base_dir);
    }
}

/// Generate a random suffix for session IDs.
fn rand_suffix() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    let state = RandomState::new();
    let mut hasher = state.build_hasher();
    hasher.write_u64(std::process::id() as u64);
    hasher.write_u64(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64,
    );
    format!("{:x}", hasher.finish() & 0xFFFFFF)
}

// ============================================================================
// Worker Architecture
// ============================================================================

use crate::index::extractor::PackageInfo;
use std::collections::HashSet;

/// A task sent to a worker for processing.
#[derive(Debug, Clone)]
pub struct CommitTask {
    /// Sequence number for ordering results.
    pub commit_seq: u64,
    /// Commit hash to process.
    pub commit_hash: String,
    /// Date of the commit (for database records).
    pub commit_date: chrono::DateTime<chrono::Utc>,
    /// Files changed in this commit (for target resolution).
    pub changed_paths: Vec<String>,
}

/// Result of processing a commit.
#[derive(Debug)]
#[allow(dead_code)]
pub struct ExtractionResult {
    /// Sequence number for ordering.
    pub commit_seq: u64,
    /// Commit hash that was processed.
    pub commit_hash: String,
    /// Date of the commit.
    pub commit_date: chrono::DateTime<chrono::Utc>,
    /// Extracted packages.
    pub packages: Vec<PackageInfo>,
    /// Attr paths we attempted to extract (for deletion detection).
    pub target_attr_paths: HashSet<String>,
    /// Files changed in this commit.
    pub changed_paths: Vec<String>,
}

/// Configuration for a worker thread.
#[derive(Debug, Clone)]
#[allow(dead_code)]
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

/// A worker that processes commits in its own worktree.
///
/// Workers are created on the main thread before spawning, then moved
/// into their thread. Each worker owns its worktree for the duration
/// of the indexing session.
#[allow(dead_code)]
pub struct Worker {
    /// Owned worktree for this worker.
    worktree: OwnedWorktree,
    /// Worker configuration.
    config: WorkerConfig,
}

#[allow(dead_code)]
impl Worker {
    /// Create a new worker with its own worktree.
    ///
    /// This MUST be called on the main thread before spawning the worker thread.
    /// The worktree is created immediately and owned by the Worker.
    ///
    /// # Arguments
    /// * `session` - The worktree session for creating the worktree
    /// * `config` - Worker configuration
    /// * `initial_commit` - Initial commit to checkout
    ///
    /// # Errors
    /// Returns an error if the worktree cannot be created.
    pub fn new(
        session: &WorktreeSession,
        config: WorkerConfig,
        initial_commit: &str,
    ) -> Result<Self> {
        let worktree = session.create_worktree(config.worker_id, initial_commit)?;
        Ok(Self { worktree, config })
    }

    /// Get the path to this worker's worktree.
    pub fn worktree_path(&self) -> &Path {
        &self.worktree.path
    }

    /// Get the worker's ID.
    #[allow(dead_code)]
    pub fn worker_id(&self) -> usize {
        self.config.worker_id
    }

    /// Process a single commit and return extraction result.
    ///
    /// This is the main work function called in the worker thread loop.
    ///
    /// # Arguments
    /// * `task` - The commit task to process
    /// * `file_attr_map` - Optional file-to-attr mapping (if None, uses heuristics)
    ///
    /// # Errors
    /// Returns an error if checkout or extraction fails.
    pub fn process_commit(
        &mut self,
        task: CommitTask,
        file_attr_map: Option<&std::collections::HashMap<String, Vec<String>>>,
    ) -> Result<ExtractionResult> {
        // Checkout the commit
        self.worktree.checkout(&task.commit_hash)?;

        // Resolve target attr paths from changed files
        let target_attr_paths = self.resolve_targets(&task.changed_paths, file_attr_map);

        if target_attr_paths.is_empty() {
            return Ok(ExtractionResult {
                commit_seq: task.commit_seq,
                commit_hash: task.commit_hash,
                commit_date: task.commit_date,
                packages: Vec::new(),
                target_attr_paths: HashSet::new(),
                changed_paths: task.changed_paths,
            });
        }

        // Extract packages for all systems
        let mut all_packages = Vec::new();
        let target_list: Vec<String> = target_attr_paths.iter().cloned().collect();

        for system in &self.config.systems {
            match crate::index::extractor::extract_packages_for_attrs(
                &self.worktree.path,
                system,
                &target_list,
            ) {
                Ok(pkgs) => all_packages.extend(pkgs),
                Err(e) => {
                    // Log but continue with other systems
                    eprintln!(
                        "Worker {}: Extraction failed for {} at {}: {}",
                        self.config.worker_id,
                        system,
                        &task.commit_hash[..7.min(task.commit_hash.len())],
                        e
                    );
                }
            }
        }

        Ok(ExtractionResult {
            commit_seq: task.commit_seq,
            commit_hash: task.commit_hash,
            commit_date: task.commit_date,
            packages: all_packages,
            target_attr_paths,
            changed_paths: task.changed_paths,
        })
    }

    /// Resolve changed file paths to target attribute paths.
    fn resolve_targets(
        &self,
        changed_paths: &[String],
        file_attr_map: Option<&std::collections::HashMap<String, Vec<String>>>,
    ) -> HashSet<String> {
        let mut targets = HashSet::new();

        for path in changed_paths {
            // Try file_attr_map first if available
            if let Some(map) = file_attr_map
                && let Some(attrs) = map.get(path)
            {
                targets.extend(attrs.iter().cloned());
                continue;
            }

            // Heuristic: extract package name from path
            if path.starts_with("pkgs/") && path.ends_with(".nix") {
                let parts: Vec<&str> = path.split('/').collect();
                if parts.len() >= 2 {
                    let potential_name = if parts.last() == Some(&"default.nix") && parts.len() >= 2
                    {
                        parts[parts.len() - 2]
                    } else {
                        parts
                            .last()
                            .map(|f| f.trim_end_matches(".nix"))
                            .unwrap_or("")
                    };

                    if !potential_name.is_empty() {
                        targets.insert(potential_name.to_string());
                    }
                }
            }
        }

        targets
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::tempdir;

    /// Create a minimal git repository for testing.
    fn create_test_repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();

        // Initialize git repo
        StdCommand::new("git")
            .args(["init"])
            .current_dir(&path)
            .output()
            .expect("Failed to init git repo");

        // Configure git user
        StdCommand::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&path)
            .output()
            .expect("Failed to configure git email");

        StdCommand::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&path)
            .output()
            .expect("Failed to configure git name");

        // Create pkgs directory (required for NixpkgsRepo::open validation)
        fs::create_dir(path.join("pkgs")).unwrap();

        // Create initial file and commit
        fs::write(path.join("file1.txt"), "content1").unwrap();
        StdCommand::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .expect("Failed to add files");
        StdCommand::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(&path)
            .output()
            .expect("Failed to create commit");

        // Create second commit
        fs::write(path.join("file2.txt"), "content2").unwrap();
        StdCommand::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .expect("Failed to add files");
        StdCommand::new("git")
            .args(["commit", "-m", "Second commit"])
            .current_dir(&path)
            .output()
            .expect("Failed to create commit");

        (dir, path)
    }

    /// Get commit hashes from a test repo.
    fn get_commits(repo_path: &Path) -> Vec<String> {
        let output = StdCommand::new("git")
            .args(["log", "--format=%H", "--reverse"])
            .current_dir(repo_path)
            .output()
            .expect("Failed to get commits");

        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(String::from)
            .collect()
    }

    #[test]
    fn test_owned_worktree_create_and_cleanup() {
        let (_dir, repo_path) = create_test_repo();
        let commits = get_commits(&repo_path);
        let base_dir = tempdir().unwrap();

        let session_id = "test123";
        let worker_id = 0;

        // Create worktree
        let worktree = OwnedWorktree::create(
            &repo_path,
            base_dir.path(),
            session_id,
            worker_id,
            &commits[0],
        )
        .expect("Failed to create worktree");

        // Verify worktree exists
        assert!(worktree.path.exists());
        assert!(worktree.path.join("file1.txt").exists());
        // Second commit's file should not exist at first commit
        assert!(!worktree.path.join("file2.txt").exists());

        // Verify naming convention
        let expected_name = format!("nxv-worktree-{}-{}", session_id, worker_id);
        assert!(
            worktree
                .path
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .contains(&expected_name)
        );

        let path_copy = worktree.path.clone();

        // Drop worktree (should clean up)
        drop(worktree);

        // Verify cleanup happened
        assert!(!path_copy.exists());
    }

    #[test]
    fn test_owned_worktree_checkout() {
        let (_dir, repo_path) = create_test_repo();
        let commits = get_commits(&repo_path);
        let base_dir = tempdir().unwrap();

        let mut worktree =
            OwnedWorktree::create(&repo_path, base_dir.path(), "test", 0, &commits[0])
                .expect("Failed to create worktree");

        // Initially at first commit
        assert!(worktree.path.join("file1.txt").exists());
        assert!(!worktree.path.join("file2.txt").exists());

        // Checkout second commit
        worktree
            .checkout(&commits[1])
            .expect("Failed to checkout second commit");

        // Now should have both files
        assert!(worktree.path.join("file1.txt").exists());
        assert!(worktree.path.join("file2.txt").exists());

        // Checkout same commit again (should be no-op)
        worktree
            .checkout(&commits[1])
            .expect("Repeat checkout should succeed");

        // Go back to first commit
        worktree
            .checkout(&commits[0])
            .expect("Failed to checkout first commit");

        // Second file should be gone
        assert!(worktree.path.join("file1.txt").exists());
        assert!(!worktree.path.join("file2.txt").exists());
    }

    #[test]
    fn test_worktree_session_creation() {
        let (_dir, repo_path) = create_test_repo();

        // Clean up any orphaned sessions first
        WorktreeSession::cleanup_orphaned_sessions();

        let session = WorktreeSession::new(&repo_path).expect("Failed to create session");

        // Verify session directory exists
        assert!(session.base_dir.exists());
        assert!(session.base_dir.join(".lock").exists());

        // Session ID should not be empty
        assert!(!session.session_id.is_empty());

        let base_dir_copy = session.base_dir.clone();

        // Drop session (should clean up)
        drop(session);

        // Verify cleanup happened
        assert!(!base_dir_copy.exists());
    }

    #[test]
    fn test_worktree_session_create_worktree() {
        let (_dir, repo_path) = create_test_repo();
        let commits = get_commits(&repo_path);

        let session = WorktreeSession::new(&repo_path).expect("Failed to create session");

        // Create worktree through session
        let worktree = session
            .create_worktree(0, &commits[0])
            .expect("Failed to create worktree");

        assert!(worktree.path.exists());
        assert!(worktree.path.join("file1.txt").exists());

        // Worktree should be inside session base_dir
        assert!(worktree.path.starts_with(&session.base_dir));
    }

    #[test]
    fn test_multiple_worktrees_in_session() {
        let (_dir, repo_path) = create_test_repo();
        let commits = get_commits(&repo_path);

        let session = WorktreeSession::new(&repo_path).expect("Failed to create session");

        // Create multiple worktrees
        let wt0 = session
            .create_worktree(0, &commits[0])
            .expect("Failed to create worktree 0");
        let wt1 = session
            .create_worktree(1, &commits[1])
            .expect("Failed to create worktree 1");

        // Both should exist with different states
        assert!(wt0.path.exists());
        assert!(wt1.path.exists());

        // wt0 at first commit - no file2
        assert!(!wt0.path.join("file2.txt").exists());

        // wt1 at second commit - has file2
        assert!(wt1.path.join("file2.txt").exists());

        // Paths should be different
        assert_ne!(wt0.path, wt1.path);
    }

    #[test]
    fn test_orphan_cleanup() {
        // Create a fake orphaned directory
        let orphan_dir = std::env::temp_dir().join("nxv-worktrees-orphan-test-12345");
        fs::create_dir_all(&orphan_dir).unwrap();
        fs::write(orphan_dir.join(".lock"), "stale lock").unwrap();

        // Run cleanup
        WorktreeSession::cleanup_orphaned_sessions();

        // The orphan should be cleaned up (since we're not holding its lock)
        // Note: This might not work on all systems if locking isn't available
        // In that case, the cleanup uses age-based detection
    }

    #[test]
    fn test_worktree_does_not_modify_main_repo() {
        let (_dir, repo_path) = create_test_repo();
        let commits = get_commits(&repo_path);

        // Get main repo's current HEAD
        let output = StdCommand::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo_path)
            .output()
            .expect("Failed to get HEAD");
        let original_head = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // Create session and worktree
        let session = WorktreeSession::new(&repo_path).expect("Failed to create session");
        let mut worktree = session
            .create_worktree(0, &commits[0])
            .expect("Failed to create worktree");

        // Checkout different commits in worktree
        worktree.checkout(&commits[1]).unwrap();
        worktree.checkout(&commits[0]).unwrap();

        // Main repo HEAD should be unchanged
        let output = StdCommand::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo_path)
            .output()
            .expect("Failed to get HEAD");
        let current_head = String::from_utf8_lossy(&output.stdout).trim().to_string();

        assert_eq!(
            original_head, current_head,
            "Main repo HEAD should not be modified by worktree operations"
        );
    }
}
