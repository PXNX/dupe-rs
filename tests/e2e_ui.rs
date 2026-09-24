//! Cypress-style end-to-end tests: these drive the real `DupeApp` through its
//! actual `eframe::App::ui` implementation using `egui_kittest`, which builds
//! an accessibility (AccessKit) tree from each frame and lets tests query
//! widgets by their visible label and synthesize real clicks/keypresses —
//! the same way a browser-based E2E test queries the DOM and dispatches
//! events, just for an egui window instead of a web page.

use dupe_rs::app::{DupeApp, ScanState};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use egui_material_icons::icons;
use std::fs;
use std::time::{Duration, Instant};
use tempfile::tempdir;

fn harness() -> Harness<'static, DupeApp> {
    Harness::builder().build_eframe(|cc| {
        egui_material_icons::initialize(&cc.egui_ctx);
        DupeApp::default()
    })
}

/// The scan/cancel control is the same button, its label swapping between the
/// two depending on `ScanState`; matching by role narrows past unrelated
/// labels that merely contain the word "Scan" (e.g. the empty-state hint).
fn click_scan_button(harness: &Harness<'static, DupeApp>) {
    let label = format!("{} Scan", icons::ICON_SCANNER.codepoint);
    harness.get_by_role_and_label(Role::Button, &label).click();
}

/// Steps the harness until the scan reaches `ScanState::Done`, polling the
/// background scanner thread the same way the real event loop does.
fn wait_for_scan_done(harness: &mut Harness<'static, DupeApp>) {
    let start = Instant::now();
    loop {
        harness.step();
        if matches!(harness.state().scan_state, ScanState::Done { .. }) {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "scan timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn scan_button_is_a_no_op_until_a_folder_is_added() {
    let mut harness = harness();
    harness.run();

    click_scan_button(&harness);
    harness.step();
    assert!(
        !harness.state().is_scanning(),
        "Scan must be disabled with no folders configured"
    );

    let dir = tempdir().unwrap();
    harness.state_mut().config.folders.push(dir.path().to_path_buf());
    harness.run();

    click_scan_button(&harness);
    harness.step();
    assert!(harness.state().is_scanning());
}

#[test]
fn scanning_via_ui_finds_duplicates_and_populates_the_table() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), b"duplicate payload").unwrap();
    fs::write(dir.path().join("b.txt"), b"duplicate payload").unwrap();
    fs::write(dir.path().join("unique.txt"), b"nothing else like this").unwrap();

    let mut harness = harness();
    harness.state_mut().config.folders.push(dir.path().to_path_buf());
    harness.run();

    click_scan_button(&harness);
    wait_for_scan_done(&mut harness);
    harness.run();

    assert_eq!(harness.state().groups.len(), 1);
    assert_eq!(harness.state().groups[0].files.len(), 2);
    // The duplicate's filename is rendered as a real label in the table.
    assert!(harness.query_by_label_contains("b.txt").is_some());
}

#[test]
fn select_all_then_delete_key_confirms_and_moves_files_to_trash() {
    let dir = tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    fs::write(&a, b"duplicate payload").unwrap();
    fs::write(&b, b"duplicate payload").unwrap();

    let mut harness = harness();
    harness.state_mut().config.folders.push(dir.path().to_path_buf());
    harness.run();
    click_scan_button(&harness);
    wait_for_scan_done(&mut harness);
    harness.run();
    assert_eq!(harness.state().groups.len(), 1);

    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.step();
    assert_eq!(harness.state().selection.len(), 2);

    harness.key_press(egui::Key::Delete);
    harness.step();
    assert!(harness.state().delete_confirm.is_some());

    harness.get_by_label("Delete").click();
    harness.run();

    assert!(harness.state().delete_confirm.is_none());
    assert!(!a.exists());
    assert!(!b.exists());
    assert!(harness.state().groups.is_empty());
}

#[test]
fn same_folder_only_checkbox_excludes_cross_folder_matches_live() {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("a")).unwrap();
    fs::create_dir(dir.path().join("b")).unwrap();
    fs::write(dir.path().join("a/photo.jpg"), b"cross folder duplicate").unwrap();
    fs::write(dir.path().join("b/photo_copy.jpg"), b"cross folder duplicate").unwrap();

    let mut harness = harness();
    harness.state_mut().config.folders.push(dir.path().to_path_buf());
    harness.run();

    click_scan_button(&harness);
    wait_for_scan_done(&mut harness);
    harness.run();
    assert_eq!(harness.state().groups.len(), 1, "cross-folder dupes found by default");

    harness.get_by_label("Same folder only").click();
    harness.run();
    click_scan_button(&harness);
    wait_for_scan_done(&mut harness);
    harness.run();

    assert!(
        harness.state().groups.is_empty(),
        "toggling 'Same folder only' must drop the cross-folder match"
    );
}
