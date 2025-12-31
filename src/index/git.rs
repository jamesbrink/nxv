//! Git repository traversal for nixpkgs.

use crate::error::{NxvError, Result};
use chrono::{DateTime, TimeZone, Utc};
use git2::{Oid, Repository, Sort};
use std::path::{Path, PathBuf};

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
    /// Whether this worktree should be cleaned up on drop.
    cleanup: bool,
}

impl Worktree {
    /// Create a new worktree handle.
    pub fn new(path: PathBuf, cleanup: bool) -> Self {
        Self { path, cleanup }
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        if self.cleanup {
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
    pub fn get_all_commits(&self) -> Result<Vec<CommitInfo>> {
        let head = self.repo.head()?;
        let head_commit = head.peel_to_commit()?;

        self.walk_commits_from(head_commit.id(), None)
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

        self.walk_commits_from(head_commit.id(), Some(since_oid))
    }

    /// Walk commits from a starting point, optionally stopping before a given commit.
    fn walk_commits_from(&self, from: Oid, stop_before: Option<Oid>) -> Result<Vec<CommitInfo>> {
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
            commits.push(CommitInfo::from_commit(&commit));
        }

        // Reverse to get chronological order (oldest first)
        commits.reverse();
        Ok(commits)
    }

    /// Checkout a specific commit (detached HEAD).
    ///
    /// This is used for nix evaluation at a specific commit.
    /// Note: This modifies the working directory.
    pub fn checkout_commit(&self, hash: &str) -> Result<()> {
        let oid = Oid::from_str(hash).map_err(|_| {
            NxvError::Git(git2::Error::from_str(&format!(
                "Invalid commit hash: {}",
                hash
            )))
        })?;

        let commit = self.repo.find_commit(oid)?;
        let tree = commit.tree()?;

        self.repo.checkout_tree(tree.as_object(), None)?;
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

    /// Create a new worktree at the specified path, checked out to a specific commit.
    ///
    /// Worktrees allow parallel checkouts of different commits without modifying
    /// the main repository's working directory.
    pub fn create_worktree(&self, worktree_path: &Path, commit_hash: &str) -> Result<Worktree> {
        let oid = Oid::from_str(commit_hash).map_err(|_| {
            NxvError::Git(git2::Error::from_str(&format!(
                "Invalid commit hash: {}",
                commit_hash
            )))
        })?;

        // Create the worktree directory
        std::fs::create_dir_all(worktree_path)?;

        // Create a unique branch name for this worktree
        let branch_name = format!("nxv-worktree-{}", &commit_hash[..8]);

        // Add the worktree using git2
        let commit = self.repo.find_commit(oid)?;

        // Create a reference for the worktree
        let reference = self.repo.reference(
            &format!("refs/heads/{}", branch_name),
            oid,
            true,
            "nxv worktree",
        )?;

        // Add the worktree
        self.repo.worktree(
            &branch_name,
            worktree_path,
            Some(
                git2::WorktreeAddOptions::new()
                    .reference(Some(&reference))
                    .checkout_existing(true),
            ),
        )?;

        // Checkout the specific commit in the worktree
        let worktree_repo = Repository::open(worktree_path)?;
        let tree = commit.tree()?;
        worktree_repo.checkout_tree(tree.as_object(), None)?;
        worktree_repo.set_head_detached(oid)?;

        Ok(Worktree::new(worktree_path.to_path_buf(), true))
    }

    /// Create multiple worktrees for parallel processing.
    ///
    /// Returns a vector of worktrees, each checked out to a different commit.
    #[allow(dead_code)]
    pub fn create_worktrees(
        &self,
        base_path: &Path,
        commits: &[&str],
    ) -> Result<Vec<Worktree>> {
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
}
