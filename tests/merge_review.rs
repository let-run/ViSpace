use vispace::{merge_review, model::*, store::Store};
use serde_json::json;
fn pair() -> (tempfile::TempDir, Store, tempfile::TempDir, Store) {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let mut x = Store::open(a.path().join("inventory.sqlite")).unwrap();
    let mut y = Store::open(b.path().join("inventory.sqlite")).unwrap();
    x.change(|d| {
        d.save_part(Part {
            number: "A".into(),
            stock: 10,
            ..Default::default()
        })
    })
    .unwrap();
    for r in x.events_after(&Default::default()).unwrap() {
        y.replay_record(&r, None).unwrap();
    }
    (a, x, b, y)
}
#[test]
fn conflicting_fields_are_staged_resolved_once_and_survive_restart() {
    let (a, mut x, _b, mut y) = pair();
    x.change(|d| {
        d.parts[0].description = "ours".into();
        Ok(())
    })
    .unwrap();
    y.change(|d| {
        d.parts[0].description = "theirs".into();
        Ok(())
    })
    .unwrap();
    let applied = x.local_head_hash();
    assert_eq!(applied, x.action_log_page(None, 1).unwrap().entries[0].hash);
    let left = x.events_after(&y.sync_clock()).unwrap();
    let right = y.events_after(&x.sync_clock()).unwrap();
    for r in left {
        y.replay_record(&r, None).unwrap();
    }
    for r in right {
        x.replay_record(&r, None).unwrap();
    }
    assert_eq!(x.data.parts[0].description, "ours");
    assert_eq!(y.data.parts[0].description, "theirs");
    assert_eq!(x.merge_reviews().len(), 1);
    let id = x.merge_reviews()[0].id.clone();
    drop(x);
    let mut x = Store::open(a.path().join("inventory.sqlite")).unwrap();
    assert_eq!(x.local_head_hash(), applied);
    assert_eq!(x.merge_reviews()[0].id, id);
    assert_eq!(x.data.parts[0].description, "ours");
    assert!(x.change(|_| Ok(())).is_err());
    x.resolve_merge(&id, json!("chosen")).unwrap();
    assert!(x.merge_reviews().is_empty());
    assert_eq!(x.data.parts[0].description, "chosen");
    for r in x.events_after(&y.sync_clock()).unwrap() {
        y.replay_record(&r, None).unwrap();
    }
    assert!(y.merge_reviews().is_empty());
    assert_eq!(y.data.parts[0].description, "chosen");
    assert_eq!(x.local_head_hash(), y.local_head_hash());
    assert_ne!(x.local_head_hash(), applied);
    assert_eq!(
        x.local_head_hash(),
        x.action_log_page(None, 1).unwrap().entries[0].hash
    );
    assert_eq!(x.merge_head().hash, y.merge_head().hash);
}
#[test]
fn markdown_diff_handles_disjoint_edits_and_marks_overlaps() {
    assert_eq!(
        merge_review::merge_text("a\nb\nc", "A\nb\nc", "a\nb\nC"),
        "A\nb\nC"
    );
    assert!(merge_review::merge_text("a", "ours", "theirs").contains("<<<<<<< OURS"));
}
#[test]
fn conflicting_stock_choices_set_the_selected_total() {
    let (_a, mut x, _b, mut y) = pair();
    let base = x.sync_clock();
    x.change(|d| {
        d.parts[0].stock = 8;
        Ok(())
    })
    .unwrap();
    y.change(|d| {
        d.parts[0].stock = 9;
        Ok(())
    })
    .unwrap();
    for r in y.events_after(&base).unwrap() {
        x.replay_record(&r, None).unwrap();
    }
    let review = x.merge_reviews().remove(0);
    assert_eq!(review.field, "stock");
    x.resolve_merge(&review.id, json!(9)).unwrap();
    assert_eq!(x.data.parts[0].stock, 9);
}
#[test]
fn markdown_nonoverlap_is_automatically_merged_and_published() {
    let (_a, mut x, _b, mut y) = pair();
    x.change(|d| {
        d.parts[0].notes.text = "a\nb\nc\n".into();
        Ok(())
    })
    .unwrap();
    for r in x.events_after(&y.sync_clock()).unwrap() {
        y.replay_record(&r, None).unwrap();
    }
    let base = x.sync_clock();
    x.change(|d| {
        d.parts[0].notes.text = "A\nb\nc\n".into();
        Ok(())
    })
    .unwrap();
    y.change(|d| {
        d.parts[0].notes.text = "a\nb\nC\n".into();
        Ok(())
    })
    .unwrap();
    for r in y.events_after(&base).unwrap() {
        x.replay_record(&r, None).unwrap();
    }
    assert!(x.merge_reviews().is_empty());
    assert_eq!(x.data.parts[0].notes.text, "A\nb\nC\n");
}
#[test]
fn delete_against_edit_offers_restore_without_adding_stock_twice() {
    let (_a, mut x, _b, mut y) = pair();
    let base = x.sync_clock();
    let id = x.data.parts[0].id.clone();
    x.change(|d| {
        d.parts.clear();
        Ok(())
    })
    .unwrap();
    y.change(|d| {
        d.parts[0].description = "edited".into();
        Ok(())
    })
    .unwrap();
    for r in y.events_after(&base).unwrap() {
        x.replay_record(&r, None).unwrap();
    }
    let review = x.merge_reviews().remove(0);
    assert_eq!(review.field, "$entity");
    x.resolve_merge(&review.id, review.theirs).unwrap();
    assert_eq!(x.data.parts[0].id, id);
    assert_eq!(x.data.parts[0].stock, 10);
}
#[test]
fn pending_branch_is_not_published_and_draft_is_durable() {
    let (a, mut x, _b, mut y) = pair();
    let base = x.sync_clock();
    x.change(|d| {
        d.parts[0].description = "ours".into();
        Ok(())
    })
    .unwrap();
    y.change(|d| {
        d.parts[0].description = "theirs".into();
        Ok(())
    })
    .unwrap();
    for r in y.events_after(&base).unwrap() {
        x.replay_record(&r, None).unwrap();
    }
    assert!(x.events_after(&base).unwrap().is_empty());
    let review = x.merge_reviews().remove(0);
    x.save_merge_draft(&review.id, "unfinished custom result")
        .unwrap();
    drop(x);
    let x = Store::open(a.path().join("inventory.sqlite")).unwrap();
    assert_eq!(
        x.merge_draft(&review.id).as_deref(),
        Some("unfinished custom result")
    );
    assert_ne!(x.shared_head().hash, x.merge_head().hash);
}
#[test]
fn stock_resolution_uses_the_latest_merged_members_not_an_old_creation_record() {
    for _ in 0..6 {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let mut x = Store::open(a.path().join("inventory.sqlite")).unwrap();
        let mut y = Store::open(b.path().join("inventory.sqlite")).unwrap();
        for (s, stock) in [(&mut x, 5), (&mut y, 7)] {
            s.change(|d| {
                d.save_part(Part {
                    number: "Shared".into(),
                    stock,
                    ..Default::default()
                })
            })
            .unwrap();
        }
        let left = x.events_after(&Default::default()).unwrap();
        let right = y.events_after(&Default::default()).unwrap();
        for r in left {
            y.replay_record(&r, None).unwrap();
        }
        for r in right {
            x.replay_record(&r, None).unwrap();
        }
        let base = x.sync_clock();
        x.change(|d| {
            d.parts[0].stock += 2;
            Ok(())
        })
        .unwrap();
        y.change(|d| {
            d.parts[0].stock += 3;
            Ok(())
        })
        .unwrap();
        for r in y.events_after(&base).unwrap() {
            x.replay_record(&r, None).unwrap();
        }
        let review = x.merge_reviews().remove(0);
        assert_eq!(review.base, json!(12));
        x.resolve_merge(&review.id, json!(17)).unwrap();
        assert_eq!(x.data.parts[0].stock, 17);
    }
}

#[test]
fn shared_resolver_projects_imported_history_before_linking_its_predecessor() {
    let (_a, mut x, _b, mut y) = pair();
    let mut mesh = vispace::mesh::Mesh::default();
    for record in x.events_after(&Default::default()).unwrap() {
        mesh.insert(record).unwrap();
    }
    for (store, text) in [(&mut x, "ours"), (&mut y, "theirs")] {
        store
            .change(|d| {
                d.parts[0].description = text.into();
                Ok(())
            })
            .unwrap();
        for record in store.events_after(&Default::default()).unwrap() {
            mesh.insert(record).unwrap();
        }
    }
    assert!(mesh.clock.is_empty()); // The browser reconstructs from the index without cached projection state.
    let review = merge_review::conflicts(&mesh.events, x.application_id()).remove(0);
    let record = merge_review::resolve(
        &mesh,
        std::path::Path::new("/browser/Attachments"),
        x.application_id(),
        &review.id,
        json!("chosen"),
    )
    .unwrap();
    mesh.insert(record).unwrap();
    let projected = mesh
        .project(std::path::Path::new("/browser/Attachments"))
        .unwrap();
    assert_eq!(projected.parts[0].description, "chosen");
    assert!(merge_review::conflicts(&mesh.events, x.application_id()).is_empty());
}

#[test]
fn proposal_rejects_stale_fields_and_keeps_independent_edits() {
    let (_a, mut leader, _b, mut client) = pair();
    let base = client.sync_clock();
    leader
        .change(|data| {
            data.parts[0].description = "committed".into();
            Ok(())
        })
        .unwrap();
    client
        .change(|data| {
            data.parts[0].description = "draft".into();
            data.parts[0].stock = 11;
            Ok(())
        })
        .unwrap();
    let committed = leader.events_after(&base).unwrap();
    let proposal = client.events_after(&base).unwrap().pop().unwrap();
    let mutations = &proposal.mesh.unwrap().mutations;
    assert_eq!(
        merge_review::stale_fields(&base, mutations, &committed),
        [(
            "parts".into(),
            mutations[0].key.clone(),
            "description".into()
        )]
        .into()
    );
    assert!(merge_review::stale_fields(&leader.sync_clock(), mutations, &committed).is_empty());
}
