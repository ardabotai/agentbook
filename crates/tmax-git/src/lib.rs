use anyhow::Result;
use git2::{BranchType, Repository, StatusOptions, WorktreeAddOptions, WorktreePruneOptions};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitMetadata {
    pub repo_root: PathBuf,
    pub worktree_path: PathBuf,
    pub branch: Option<String>,
    pub is_dirty: bool,
}

pub fn detect_git_metadata(cwd: &Path) -> Result<Option<GitMetadata>> {
    let repo = match Repository::discover(cwd) {
        Ok(repo) => repo,
        Err(_) => return Ok(None),
    };

    let repo_root = repo
        .workdir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| repo.path().to_path_buf());

    let worktree_path = repo_root.clone();
    let branch = repo
        .head()
        .ok()
        .and_then(|head| head.shorthand().map(ToOwned::to_owned));

    let is_dirty = detect_dirty(&repo)?;

    Ok(Some(GitMetadata {
        repo_root,
        worktree_path,
        branch,
        is_dirty,
    }))
}

pub fn detect_dirty(repo: &Repository) -> Result<bool> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .include_ignored(false)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true)
        .recurse_untracked_dirs(true);

    let statuses = repo.statuses(Some(&mut opts))?;
    Ok(!statuses.is_empty())
}

pub fn create_worktree_for_branch(repo_cwd: &Path, branch: &str) -> Result<PathBuf> {
    let repo = Repository::discover(repo_cwd)?;
    let repo_root = repo
        .workdir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| repo.path().to_path_buf());

    let branch_ref = ensure_local_branch(&repo, branch)?;
    let worktree_root = repo_root.join(".tmax-worktrees");
    std::fs::create_dir_all(&worktree_root)?;

    let branch_slug = sanitize_name(branch);
    let (name, worktree_path) = next_available_worktree(&worktree_root, &branch_slug);

    let mut opts = WorktreeAddOptions::new();
    opts.reference(Some(&branch_ref));
    let _ = repo.worktree(&name, &worktree_path, Some(&opts))?;

    Ok(worktree_path)
}

pub fn clean_worktree(repo_root: &Path, worktree_path: &Path) -> Result<()> {
    let repo = Repository::open(repo_root)?;
    let entries = repo.worktrees()?;

    for name in entries.iter().flatten() {
        let wt = repo.find_worktree(name)?;
        if canonical_eq(wt.path(), worktree_path) {
            let mut opts = WorktreePruneOptions::new();
            opts.valid(true).locked(true).working_tree(true);
            wt.prune(Some(&mut opts))?;
            return Ok(());
        }
    }

    if worktree_path.exists() {
        std::fs::remove_dir_all(worktree_path)?;
    }
    Ok(())
}

fn ensure_local_branch<'repo>(
    repo: &'repo Repository,
    branch: &str,
) -> Result<git2::Reference<'repo>> {
    if let Ok(existing) = repo.find_branch(branch, BranchType::Local) {
        return Ok(existing.into_reference());
    }

    let commit = repo.head()?.peel_to_commit()?;
    let created = repo.branch(branch, &commit, false)?;
    Ok(created.into_reference())
}

fn next_available_worktree(root: &Path, branch_slug: &str) -> (String, PathBuf) {
    let base = format!("tmax-{branch_slug}");
    let mut n = 0usize;
    loop {
        let suffix = if n == 0 {
            String::new()
        } else {
            format!("-{n}")
        };
        let name = format!("{base}{suffix}");
        let path = root.join(&name);
        if !path.exists() {
            return (name, path);
        }
        n += 1;
    }
}

fn sanitize_name(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_lowercase()
}

fn canonical_eq(left: &Path, right: &Path) -> bool {
    let a = std::fs::canonicalize(left).ok();
    let b = std::fs::canonicalize(right).ok();
    match (a, b) {
        (Some(a), Some(b)) => a == b,
        _ => left == right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn returns_none_outside_git_repo() {
        let dir = tempdir().expect("tempdir");
        let meta = detect_git_metadata(dir.path()).expect("detect");
        assert!(meta.is_none());
    }

    #[test]
    fn detects_repo_branch_and_dirty_state() {
        let dir = tempdir().expect("tempdir");
        let repo = Repository::init(dir.path()).expect("init repo");

        std::fs::write(dir.path().join("README.md"), "hello").expect("write file");
        let meta = detect_git_metadata(dir.path())
            .expect("detect")
            .expect("metadata expected");

        let expected = std::fs::canonicalize(dir.path()).expect("canonical");
        let repo_root = std::fs::canonicalize(&meta.repo_root).expect("canonical repo root");
        let worktree = std::fs::canonicalize(&meta.worktree_path).expect("canonical worktree");
        assert_eq!(repo_root, expected);
        assert_eq!(worktree, expected);
        assert!(meta.is_dirty);

        drop(repo);
    }

    #[test]
    fn create_and_clean_worktree_round_trip() {
        let dir = tempdir().expect("tempdir");
        let repo = Repository::init(dir.path()).expect("init repo");
        std::fs::write(dir.path().join("README.md"), "hello").expect("write readme");

        let mut index = repo.index().expect("index");
        index
            .add_path(Path::new("README.md"))
            .expect("add path to index");
        let tree_id = index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_id).expect("find tree");
        let sig = git2::Signature::now("tmax", "tmax@example.com").expect("sig");
        let _ = repo
            .commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .expect("commit");

        let wt = create_worktree_for_branch(dir.path(), "feature/demo").expect("create worktree");
        assert!(wt.exists(), "worktree path should exist");

        clean_worktree(dir.path(), &wt).expect("clean worktree");
        assert!(!wt.exists(), "worktree path should be removed");
    }
}
