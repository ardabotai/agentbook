use anyhow::Result;
use git2::{Repository, Signature};
use std::path::Path;
use tmax_git::{clean_worktree, create_worktree_for_branch, detect_git_metadata};

#[test]
fn integration_detects_and_manages_worktree() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let repo = Repository::init(dir.path())?;
    std::fs::write(dir.path().join("README.md"), "hello")?;

    let mut index = repo.index()?;
    index.add_path(Path::new("README.md"))?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    let sig = Signature::now("tmax", "tmax@example.com")?;
    let _ = repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])?;

    let meta = detect_git_metadata(dir.path())?.expect("expected metadata");
    assert!(meta.branch.is_some(), "branch should be detected");

    let worktree_path = create_worktree_for_branch(dir.path(), "feature/integration")?;
    assert!(worktree_path.exists(), "worktree path should exist");

    clean_worktree(dir.path(), &worktree_path)?;
    assert!(!worktree_path.exists(), "worktree path should be removed");

    Ok(())
}
