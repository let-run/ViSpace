//! Isolated native peer for the real-browser integration test; JSON commands over stdin.
use vispace::{model::Inventory, store::Store, sync::SyncServer};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};
fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(std::env::args().nth(1).expect("temporary test directory"));
    let store = Arc::new(Mutex::new(Store::open(root.join("inventory.sqlite"))?));
    vispace::component_sync::start(&store)?;
    if std::env::args().nth(2).as_deref() == Some("--partition-sync") {
        Store::enable_component(&store)?;
    }
    let server = SyncServer::start(store.clone(), "127.0.0.1:0".parse()?)?;
    let address = server.address;
    println!("{}", json!({"url":format!("wss://localhost:{}",address.port())}));
    io::stdout().flush()?;
    for line in io::stdin().lock().lines() {
        let response = (|| -> anyhow::Result<Value> {
            let command: Value = serde_json::from_str(&line?)?;
            let mut s = store.lock().unwrap();
            match command["op"].as_str().unwrap_or("") {
                "security"=>Ok(json!({"id":s.application_id(),"public_key":s.network_identity().public_key()?,"pending":s.network_pending(),"actions":s.network_actions()})),
                "authorize"=>{s.network_authorize(command["public_key"].as_str().unwrap().into(),false)?;Ok(json!(true))},
                "revoke"=>{s.network_authorize(command["public_key"].as_str().unwrap().into(),true)?;Ok(json!(true))},
                "enable_partition"=>{drop(s);Store::enable_component(&store)?;Ok(json!(true))},
                "get" => Ok(
                    json!({"inventory":s.data,"reviews":s.merge_reviews(),"head":s.merge_head(),"appliedHead":s.local_head_hash(),"applicationId":s.application_id(),"topology":s.network_graph()}),
                ),
                "set" => {
                    let next: Inventory = serde_json::from_value(command["inventory"].clone())?;
                    s.change(|d| {
                        *d = next;
                        Ok(())
                    })?;
                    Ok(json!(true))
                }
                "commit_project"=>{
                    use grit_lib::{porcelain::commit::{commit_with_scope,CommitRequest,CommitScope},prelude::{Repository,RepositoryOptions},progress::{NullProgress,CancelToken},refs::store::RefName};
                    let p=s.data.projects.iter().find(|p|Some(p.id.as_str())==command["project"].as_str()).ok_or_else(||anyhow::anyhow!("Project missing"))?;
                    let path=std::path::Path::new(&p.kicad.as_ref().ok_or_else(||anyhow::anyhow!("Link missing"))?.path);
                    let root=path.parent().unwrap();let repo=Repository::open(&root.join(".git"),&RepositoryOptions::empty().with_work_tree(root))?;
                    if let Some(base)=command["base"].as_str(){repo.references().update_direct(&RefName::try_from("refs/heads/main")?,&grit_lib::objects::ObjectId::from_hex(base)?)?;}
                    repo.config().set_local("user.name","Desktop Test")?;repo.config().set_local("user.email","desktop@example.invalid")?;
                    std::fs::write(path,command["text"].as_str().unwrap())?;
                    commit_with_scope(&repo,&CommitRequest::new("Desktop received checkout update"),CommitScope::AutoStage,&mut NullProgress,&CancelToken::never())?;
                    Ok(json!({"tip":repo.references().resolve(RefName::try_from("HEAD")?)?.to_hex()}))
                }
                "merge_project"=>{
                    use grit_lib::{objects::{ObjectId,ObjectKind,serialize_commit},prelude::{Repository,RepositoryOptions},refs::store::RefName};
                    let p=s.data.projects.iter().find(|p|Some(p.id.as_str())==command["project"].as_str()).ok_or_else(||anyhow::anyhow!("Project missing"))?;
                    let root=std::path::Path::new(&p.kicad.as_ref().unwrap().path).parent().unwrap();let repo=Repository::open(&root.join(".git"),&RepositoryOptions::empty().with_work_tree(root))?;
                    let branch=RefName::try_from("refs/heads/main")?;let ours=repo.references().resolve(&branch)?;let other=ObjectId::from_hex(command["other"].as_str().unwrap())?;
                    let incoming=RefName::try_from(format!("refs/vinventory/bundles/{}",other.to_hex()).as_str())?;
                    anyhow::ensure!(repo.references().resolve(&incoming)?==other,"Incoming history not retained in refs");
                    let mut commit=repo.odb().read_commit(&ours)?;commit.parents=vec![ours,other];commit.message="Manually resolved concurrent histories".into();commit.raw_message=None;
                    let merged=repo.odb().write(ObjectKind::Commit,&serialize_commit(&commit))?;repo.references().update_direct(&branch,&merged)?;
                    Ok(json!({"tip":merged.to_hex()}))
                }
                "asset" => {
                    let relative = command["path"].as_str().unwrap();
                    anyhow::ensure!(
                        !relative.contains("..") && !relative.starts_with('/'),
                        "Unsafe test path"
                    );
                    let path = root.join("Attachments").join(relative);
                    std::fs::create_dir_all(path.parent().unwrap())?;
                    std::fs::write(
                        &path,
                        command["bytes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|v| v.as_u64().unwrap() as u8)
                            .collect::<Vec<_>>(),
                    )?;
                    Ok(json!(path.to_string_lossy()))
                }
                "resolve" => {
                    s.resolve_merge(command["id"].as_str().unwrap(), command["value"].clone())?;
                    Ok(json!(true))
                }
                _ => anyhow::bail!("Unknown test command"),
            }
        })();
        println!(
            "{}",
            match response {
                Ok(v) => json!({"value":v}),
                Err(e) => json!({"error":e.to_string()}),
            }
        );
        io::stdout().flush()?;
    }
    drop(server);
    Ok(())
}
