//! Git repository traversal for nixpkgs.

use crate::error::{NxvError, Result};
use chrono::{DateTime, TimeZone, Utc};
use git2::{Oid, Repository, Sort};
use std::path::{Path, PathBuf};
use std::process::Command;

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
    pub fn get_indexable_commits_touching_paths(
        &self,
        paths: &[&str],
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<CommitInfo>> {
        let since_arg = since.unwrap_or(MIN_INDEXABLE_DATE);
        let mut args = vec!["log", "--first-parent", "--format=%H", "--since", since_arg];
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
    pub fn get_commit_changed_paths(&self, commit_hash: &str) -> Result<Vec<String>> {
        let output = Command::new("git")
            .current_dir(&self.path)
            .args(["diff-tree", "--name-status", "-r", commit_hash])
            .output()?;

        if !output.status.success() {
            return Err(NxvError::Git(git2::Error::from_str(
                "Failed to list commit changes.",
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut paths = Vec::new();
        for line in stdout.lines() {
            let mut parts = line.split('\t');
            let status = parts.next().unwrap_or_default();
            if status.starts_with('R') {
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

    /// Checkout a specific commit (detached HEAD) with force.
    ///
    /// This is used for nix evaluation at a specific commit.
    /// Note: This modifies the working directory.
    /// Uses force checkout to handle conflicts when switching between
    /// commits with very different file structures.
    pub fn checkout_commit(&self, hash: &str) -> Result<()> {
        let oid = Oid::from_str(hash).map_err(|_| {
            NxvError::Git(git2::Error::from_str(&format!(
                "Invalid commit hash: {}",
                hash
            )))
        })?;

        let commit = self.repo.find_commit(oid)?;
        let tree = commit.tree()?;

        // Use force checkout to avoid conflicts
        let mut checkout_opts = git2::build::CheckoutBuilder::new();
        checkout_opts.force();
        checkout_opts.remove_untracked(true);

        self.repo
            .checkout_tree(tree.as_object(), Some(&mut checkout_opts))?;
        self.repo.set_head_detached(oid)?;

        Ok(())
    }

    /// Get the current HEAD commit hash.
    #[allow(dead_code)]
    pub fn head_commit(&self) -> Result<String> {
        let head = self.repo.head()?;
        let commit = head.peel_to_commit()?;
        Ok(commit.id().to_string())
    }

    /// Get the current HEAD reference name (branch name or commit hash if detached).
    ///
    /// Returns the branch name (e.g., "refs/heads/master") or commit hash if detached.
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

    /// Restore the repository to a specific ref (branch or commit).
    ///
    /// If ref_name is a branch reference (starts with "refs/"), checkout that branch.
    /// Otherwise, treat it as a commit hash and checkout detached.
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

    /// Count the total number of commits (for progress reporting).
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

    /// Get the path to the repository.
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Check if commit A is an ancestor of commit B.
    ///
    /// Returns true if A is reachable from B (i.e., A is older than or equal to B).
    /// This is useful for detecting if the repository HEAD has been reset to
    /// an older commit than the last indexed commit.
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

    /// Clean up all nxv worktrees.
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

        if !nixpkgs_path.exists() || !nixpkgs_path.join("pkgs").exists() {
            // Skip if nixpkgs submodule isn't present
            eprintln!(
                "Skipping: nixpkgs submodule not present at {:?}",
                nixpkgs_path
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
}
