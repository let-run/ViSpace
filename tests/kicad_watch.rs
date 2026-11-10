#![cfg(not(target_arch = "wasm32"))]
use vispace::kicad_watch::Watch;
use std::time::{Duration, Instant};
fn changed(watch: &Watch) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if watch.changed() {
            return;
        }
        assert!(Instant::now() < deadline, "watcher missed file replacement");
        std::thread::sleep(Duration::from_millis(25));
    }
}
#[test]
fn watcher_detects_editor_atomic_saves_deletion_and_recreation() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("design.kicad_sch");
    std::fs::write(&path, "original").unwrap();
    let watch = Watch::new(vec![path.clone()], || {}).unwrap();
    std::thread::sleep(Duration::from_millis(650));
    assert!(!watch.changed());
    let replacement = folder.path().join("temporary");
    std::fs::write(&replacement, "changed schematic").unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    changed(&watch);
    assert!(!watch.changed());
    std::fs::remove_file(&path).unwrap();
    changed(&watch);
    std::fs::write(&path, "restored").unwrap();
    changed(&watch);
}

#[test]
fn pausing_and_resuming_only_reports_actual_intervening_changes() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("design.kicad_sch");
    std::fs::write(&path, "original").unwrap();
    let watch = Watch::new(vec![path.clone()], || {}).unwrap();
    std::thread::sleep(Duration::from_millis(650));
    watch.set_active(false);
    std::thread::sleep(Duration::from_millis(650));
    watch.set_active(true);
    std::thread::sleep(Duration::from_millis(1100));
    assert!(
        !watch.changed(),
        "unchanged project must not reload on returning"
    );
    watch.set_active(false);
    std::fs::write(&path, "modified while another tab was open").unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    assert!(!watch.changed(), "inactive project must not be monitored");
    watch.set_active(true);
    changed(&watch);
    assert!(!watch.changed());
}
