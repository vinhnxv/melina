//! Git context detection — branch, dirty state, worktree awareness.
//!
//! Uses git2 (libgit2) to inspect the git repository at a given path.

use serde::Serialize;
use std::path::Path;

/// Git context for a Claude session's working directory.
#[derive(Debug, Clone, Serialize)]
pub struct GitContext {
    /// Current branch name (or short commit hash if detached HEAD).
    pub branch: String,
    /// Whether there are any uncommitted changes (staged or unstaged).
    pub is_dirty: bool,
    /// Whether this directory is a git worktree (not the main checkout).
    pub is_worktree: bool,
    /// Commits ahead of upstream (0 if no upstream).
    pub ahead: usize,
    /// Commits behind upstream (0 if no upstream).
    pub behind: usize,
}

impl GitContext {
    /// Detect git context for a given path. Returns None if not a git repo.
    pub fn detect(path: &Path) -> Option<Self> {
        let repo = git2::Repository::discover(path).ok()?;

        // Skip bare repositories
        if repo.is_bare() {
            return None;
        }

        // Get branch name
        let branch = match repo.head() {
            Ok(head) => {
                if head.is_branch() {
                    head.shorthand().unwrap_or("HEAD").to_string()
                } else {
                    // Detached HEAD — show short commit hash
                    head.peel_to_commit()
                        .map(|c| c.id().to_string()[..7].to_string())
                        .unwrap_or_else(|_| "HEAD".to_string())
                }
            }
            Err(_) => "HEAD".to_string(),
        };

        // Check dirty state (staged or unstaged changes)
        let is_dirty = check_dirty(&repo);

        // Check if worktree
        let is_worktree = repo.is_worktree();

        // Get ahead/behind upstream
        let (ahead, behind) = get_ahead_behind(&repo);

        Some(GitContext {
            branch,
            is_dirty,
            is_worktree,
            ahead,
            behind,
        })
    }

    /// Format for display: "main *" or "feat/x ↑2↓1"
    pub fn display(&self) -> String {
        let mut parts = vec![self.branch.clone()];

        if self.is_dirty {
            parts.push("*".to_string());
        }

        if self.ahead > 0 || self.behind > 0 {
            let mut sync = String::new();
            if self.ahead > 0 {
                sync.push_str(&format!("↑{}", self.ahead));
            }
            if self.behind > 0 {
                sync.push_str(&format!("↓{}", self.behind));
            }
            parts.push(sync);
        }

        if self.is_worktree {
            parts.push("[wt]".to_string());
        }

        parts.join(" ")
    }
}

impl std::fmt::Display for GitContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display())
    }
}

fn check_dirty(repo: &git2::Repository) -> bool {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .include_ignored(false)
        .exclude_submodules(true);

    repo.statuses(Some(&mut opts))
        .map(|statuses| !statuses.is_empty())
        .unwrap_or(false)
}

fn get_ahead_behind(repo: &git2::Repository) -> (usize, usize) {
    let head = match repo.head() {
        Ok(h) if h.is_branch() => h,
        _ => return (0, 0),
    };

    let branch_name = match head.shorthand() {
        Some(n) => n.to_string(),
        None => return (0, 0),
    };

    let local_branch = match repo.find_branch(&branch_name, git2::BranchType::Local) {
        Ok(b) => b,
        Err(_) => return (0, 0),
    };

    let upstream = match local_branch.upstream() {
        Ok(u) => u,
        Err(_) => return (0, 0), // No upstream configured
    };

    let local_oid = match head.target() {
        Some(oid) => oid,
        None => return (0, 0),
    };

    let upstream_oid = match upstream.get().target() {
        Some(oid) => oid,
        None => return (0, 0),
    };

    repo.graph_ahead_behind(local_oid, upstream_oid)
        .unwrap_or((0, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_non_git_directory() {
        let dir = std::env::temp_dir();
        // temp_dir itself may or may not be a git repo — just verify no panic
        let _ = GitContext::detect(&dir);
    }

    #[test]
    fn test_display_clean() {
        let ctx = GitContext {
            branch: "main".to_string(),
            is_dirty: false,
            is_worktree: false,
            ahead: 0,
            behind: 0,
        };
        assert_eq!(ctx.display(), "main");
    }

    #[test]
    fn test_display_dirty() {
        let ctx = GitContext {
            branch: "main".to_string(),
            is_dirty: true,
            is_worktree: false,
            ahead: 0,
            behind: 0,
        };
        assert_eq!(ctx.display(), "main *");
    }

    #[test]
    fn test_display_ahead_behind() {
        let ctx = GitContext {
            branch: "feat/x".to_string(),
            is_dirty: false,
            is_worktree: false,
            ahead: 2,
            behind: 1,
        };
        assert_eq!(ctx.display(), "feat/x ↑2↓1");
    }

    #[test]
    fn test_display_worktree() {
        let ctx = GitContext {
            branch: "dev".to_string(),
            is_dirty: true,
            is_worktree: true,
            ahead: 0,
            behind: 0,
        };
        assert_eq!(ctx.display(), "dev * [wt]");
    }
}
