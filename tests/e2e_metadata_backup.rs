//! End-to-end test of the Metadata Backup tab, driving the real `DupeApp`
//! through `egui_kittest` like `e2e_ui.rs` does.

use dupe_rs::app::{AppTab, DupeApp};
use dupe_rs::metadata_backup::MetadataBackupState;
use dupe_rs::ui::dialogs::PickPurpose;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use std::fs;
use std::time::{Duration, Instant};
use tempfile::tempdir;

fn harness() -> Harness<'static, DupeApp> {
    Harness::builder().build_eframe(|cc| {
        egui_material_icons::initialize(&cc.egui_ctx);
        let mut app = DupeApp::default();
        app.play_sounds = false;
        app
    })
}

fn wait(harness: &mut Harness<'static, DupeApp>) {
    let start = Instant::now();
    while harness.state().metadata_backup.is_running() {
        harness.step();
        assert!(start.elapsed() < Duration::from_secs(20), "job timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    harness.run();
}

#[test]
fn backs_up_then_restores_a_renamed_file() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("files");
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("sub/letter.txt"), b"dear diary").unwrap();

    let mut harness = harness();
    harness.state_mut().metadata_backup =
        MetadataBackupState::with_backups_dir(dir.path().join("backups"));
    harness.run();
    harness.get_by_label_contains("Metadata Backup").click();
    harness.run();
    assert_eq!(harness.state().tab, AppTab::MetadataBackup);

    harness
        .state_mut()
        .apply_pick(PickPurpose::MetadataBackupRoot, vec![root.clone()]);
    harness.run();
    harness.get_by_label_contains("Back up metadata").click();
    harness.step();
    wait(&mut harness);
    assert!(
        harness
            .query_by_label_contains("Backed up 1 file(s)")
            .is_some()
    );

    fs::rename(root.join("sub/letter.txt"), root.join("renamed.txt")).unwrap();
    harness
        .get_by_label_contains("Compare with files now")
        .click();
    harness.step();
    wait(&mut harness);
    assert!(harness.query_by_label_contains("now renamed.txt").is_some());

    harness.get_by_label_contains("Restore").click();
    harness.step();
    wait(&mut harness);
    // Restoring compares again afterwards.
    wait(&mut harness);
    assert_eq!(
        fs::read(root.join("sub/letter.txt")).unwrap(),
        b"dear diary"
    );
    assert!(!root.join("renamed.txt").exists());
    assert!(
        harness
            .query_by_label_contains("Nothing to restore")
            .is_some()
    );
}
