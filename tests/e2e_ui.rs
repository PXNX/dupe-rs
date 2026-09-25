//! Cypress-style end-to-end tests: these drive the real `DupeApp` through its
//! actual `eframe::App::ui` implementation using `egui_kittest`, which builds
//! an accessibility (AccessKit) tree from each frame and lets tests query
//! widgets by their visible label and synthesize real clicks/keypresses —
//! the same way a browser-based E2E test queries the DOM and dispatches
//! events, just for an egui window instead of a web page.

use dupe_rs::app::{AppTab, DupeApp, ScanState, ViewMode};
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
        let mut app = DupeApp::default();
        // Keep test runs quiet.
        app.play_sounds = false;
        app
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

/// Steps the harness until the background trash-deletion thread reports it's
/// finished (the confirm dialog closed and nothing left running). Real
/// Windows Recycle Bin operations go through shell/COM machinery that can
/// serialize heavily when several tests hit it around the same time under
/// `cargo test`'s default parallelism, so this budget is generous compared to
/// the sub-second time a couple of file deletions take in isolation.
fn wait_for_delete_done(harness: &mut Harness<'static, DupeApp>) {
    let start = Instant::now();
    loop {
        harness.step();
        if harness.state().delete_confirm.is_none() && !harness.state().is_deleting() {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "delete timed out");
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
    harness.step();
    wait_for_delete_done(&mut harness);
    harness.run();

    assert!(harness.state().delete_confirm.is_none());
    assert!(!a.exists());
    assert!(!b.exists());
    assert!(harness.state().groups.is_empty());
}

#[test]
fn permanent_delete_checkbox_removes_files_without_the_trash() {
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

    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.step();
    harness.key_press(egui::Key::Delete);
    harness.step();
    harness.run();

    harness.get_by_label_contains("Delete permanently").click();
    harness.run();
    assert!(harness.state().delete_confirm.as_ref().unwrap().permanent);

    harness.get_by_label("Delete").click();
    harness.step();
    wait_for_delete_done(&mut harness);
    harness.run();

    assert!(!a.exists());
    assert!(!b.exists());
    assert!(harness.state().groups.is_empty());
    assert!(
        harness
            .state()
            .status_message
            .as_deref()
            .is_some_and(|m| m.starts_with("Permanently deleted 2 file(s)")),
    );
}

#[test]
fn a_second_delete_can_start_while_the_first_is_still_running() {
    let dir = tempdir().unwrap();
    let paths: Vec<_> = ["a1.txt", "a2.txt", "b1.txt", "b2.txt"]
        .iter()
        .map(|n| dir.path().join(n))
        .collect();
    fs::write(&paths[0], b"first duplicate payload").unwrap();
    fs::write(&paths[1], b"first duplicate payload").unwrap();
    fs::write(&paths[2], b"second duplicate payload!").unwrap();
    fs::write(&paths[3], b"second duplicate payload!").unwrap();

    let mut harness = harness();
    harness.state_mut().config.folders.push(dir.path().to_path_buf());
    harness.run();
    click_scan_button(&harness);
    wait_for_scan_done(&mut harness);
    harness.run();
    assert_eq!(harness.state().groups.len(), 2);

    for (to_delete, other) in [(&paths[1], &paths[0]), (&paths[3], &paths[2])] {
        let app = harness.state_mut();
        app.selection.insert(to_delete.clone());
        app.selection.insert(other.clone());
        app.delete_selected();
        let confirm = app.delete_confirm.as_mut().unwrap();
        // Only the file not already queued by an earlier job is included.
        confirm.paths.retain(|p| p == to_delete);
        confirm.permanent = true;
        app.confirm_delete();
        app.selection.clear();
    }
    assert!(harness.state().is_deleting());
    assert!(harness.state().delete_confirm.is_none());

    wait_for_delete_done(&mut harness);
    harness.run();

    assert!(paths[0].exists());
    assert!(!paths[1].exists());
    assert!(paths[2].exists());
    assert!(!paths[3].exists());
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

#[test]
fn bulk_selection_buttons_and_view_mode_toggle_are_clickable() {
    // Regression test: these controls once sat right after a right-aligned
    // `right_to_left` block placed *earlier* in the same row, which claims
    // the rest of the row for itself and pushes every later widget off past
    // the right edge — so clicks silently landed nowhere. Every widget here
    // must actually be clickable at its reported position.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), b"duplicate payload").unwrap();
    fs::write(dir.path().join("b.txt"), b"duplicate payload").unwrap();

    let mut harness = harness();
    harness.state_mut().config.folders.push(dir.path().to_path_buf());
    harness.run();
    click_scan_button(&harness);
    wait_for_scan_done(&mut harness);
    harness.run();

    harness
        .get_by_label_contains("Select All Shown")
        .click();
    harness.step();
    assert_eq!(harness.state().selection.len(), 2);

    harness.get_by_label_contains("Select None").click();
    harness.step();
    assert!(harness.state().selection.is_empty());

    assert_eq!(harness.state().view_mode, ViewMode::Table);
    harness.get_by_label_contains("Grid").click();
    harness.run();
    assert_eq!(harness.state().view_mode, ViewMode::Grid);
}

#[test]
fn reverse_search_tab_switches_the_central_panel_and_back() {
    let mut harness = harness();
    harness.run();
    assert_eq!(harness.state().tab, AppTab::Scan);

    harness.get_by_label_contains("Reverse Search").click();
    harness.run();
    assert_eq!(harness.state().tab, AppTab::ReverseSearch);
    assert!(harness.query_by_label_contains("Find matches for a file").is_some());
    assert!(harness.query_by_label_contains("Index folders").is_some());

    harness.get_by_label_contains("Duplicates").click();
    harness.run();
    assert_eq!(harness.state().tab, AppTab::Scan);
}

#[test]
fn reverse_search_indexes_a_folder_and_finds_a_match_by_content() {
    // Uses its own temp index file rather than the real
    // %LOCALAPPDATA%\dupe-rs\reverse_index.json — indexing a drive replaces
    // that drive's prior entries, and this test must not clobber a real
    // index a user has already built on whatever drive `TEMP` happens to be.
    let db_dir = tempdir().unwrap();
    let dir = tempdir().unwrap();
    let indexed = dir.path().join("original.txt");
    fs::write(&indexed, b"reverse search payload").unwrap();

    let mut harness = harness();
    harness.state_mut().tab = dupe_rs::app::AppTab::ReverseSearch;
    harness.state_mut().reverse_search =
        dupe_rs::reverse_search::ReverseSearchState::new(db_dir.path().join("index.json"));
    harness.state_mut().reverse_search.index_folders.push(dir.path().to_path_buf());
    harness.run();

    harness.state_mut().reverse_search.start_indexing();
    let start = Instant::now();
    loop {
        harness.step();
        if !harness.state().reverse_search.is_indexing() {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "indexing timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
    harness.run();

    assert!(harness.state().reverse_search.db.total_files() >= 1);

    let picked = dir.path().join("picked_copy.txt");
    fs::write(&picked, b"reverse search payload").unwrap();
    harness.state_mut().reverse_search.pick_file(picked);
    harness.run();

    assert_eq!(harness.state().reverse_search.results.len(), 1);
    // rel_path is relative to the volume root (not the scanned folder), so
    // the match's reconstructed absolute path should round-trip back to the
    // indexed file.
    let matched = &harness.state().reverse_search.results[0];
    assert_eq!(matched.absolute_path(), indexed);
}
