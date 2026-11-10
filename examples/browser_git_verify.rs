//! Verify browser-written Git metadata with the desktop Grit runtime.
use grit_lib::{
    prelude::{Repository, RepositoryOptions},
    refs::store::RefName,
};
fn main() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(std::env::args().nth(1).expect("test repository"));
    let repo = Repository::open(
        &root.join(".git"),
        &RepositoryOptions::empty().with_work_tree(&root),
    )?;
    let tip = repo
        .references()
        .resolve(&RefName::try_from("refs/heads/main")?)?;
    let commit = repo.objects().read_commit(&tip)?;
    let tree = repo.objects().read(&commit.tree)?;
    anyhow::ensure!(
        tree.kind == grit_lib::objects::ObjectKind::Tree,
        "HEAD tree missing"
    );
    let index = repo.index().load()?;
    anyhow::ensure!(!index.entries().is_empty(), "Git index is empty");
    println!(
        "{}",
        serde_json::json!({"tip":tip.to_hex(),"parents":commit.parents.iter().map(|id|id.to_hex()).collect::<Vec<_>>(),"files":index.entries().len()})
    );
    Ok(())
}
