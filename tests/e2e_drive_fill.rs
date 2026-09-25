//! End-to-end drive fill: measure a source's top-level folders, plan against
//! a (simulated) amount of free space, copy the plan onto a target, and find
//! the copies again through reverse search.

use dupe_rs::app::SortDirection;
use dupe_rs::drive_fill::{DriveFillState, FolderSortColumn, FolderStatus};
use dupe_rs::reverse_search::ReverseSearchState;
use dupe_rs::volume::DiskSpace;
use std::fs;
use std::time::{Duration, Instant};
use tempfile::tempdir;

fn wait_until_idle(state: &mut DriveFillState, reverse_search: &mut ReverseSearchState) {
    let start = Instant::now();
    while state.needs_polling() || reverse_search.is_looking_up() {
        reverse_search.drain_lookup();
        state.drain_events(reverse_search);
        assert!(start.elapsed() < Duration::from_secs(20), "drive fill timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn fills_the_target_with_the_best_fitting_folders_and_indexes_them() {
    let source = tempdir().unwrap();
    let target = tempdir().unwrap();
    let db_dir = tempdir().unwrap();
    // 60 KB + 50 KB + 45 KB folders against ~100 KB of room: largest-first
    // would copy only the 60 KB folder; the best fit is 50 KB + 45 KB.
    for (name, kb) in [("big", 60), ("mid", 50), ("small", 45)] {
        let dir = source.path().join(name);
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("data.bin"), vec![name.len() as u8; kb * 1000]).unwrap();
    }
    fs::create_dir(source.path().join("small/nested")).unwrap();
    fs::write(source.path().join("small/nested/note.txt"), b"hello").unwrap();

    let mut reverse_search = ReverseSearchState::new(db_dir.path().join("index.json"));
    let mut state = DriveFillState::default();
    state.set_source(source.path().to_path_buf());
    wait_until_idle(&mut state, &mut reverse_search);
    assert_eq!(state.folders.len(), 3);

    let names = |state: &DriveFillState| -> Vec<String> {
        state
            .sorted_indices()
            .into_iter()
            .map(|i| state.folders[i].name.clone())
            .collect()
    };
    // Largest first by default, and the overview re-sorts by any column.
    assert_eq!(names(&state), ["big", "mid", "small"]);
    state.sort = Some((FolderSortColumn::Name, SortDirection::Desc));
    assert_eq!(names(&state), ["small", "mid", "big"]);
    state.sort = Some((FolderSortColumn::Files, SortDirection::Desc));
    assert_eq!(names(&state)[0], "small");
    state.sort = None;

    state.set_target(target.path().to_path_buf());
    wait_until_idle(&mut state, &mut reverse_search);
    state.reserve_mb = 0;
    state.space = Some(DiskSpace {
        free: 105_000,
        total: 1_000_000,
        cluster: 1,
    });
    state.replan();

    let status_of = |state: &DriveFillState, name: &str| {
        let i = state.folders.iter().position(|f| f.name == name).unwrap();
        state.plan.statuses[i]
    };
    assert_eq!(status_of(&state, "big"), FolderStatus::DoesNotFit);
    assert_eq!(status_of(&state, "mid"), FolderStatus::Planned);
    assert_eq!(status_of(&state, "small"), FolderStatus::Planned);

    state.start_copy();
    assert!(state.is_copying());
    wait_until_idle(&mut state, &mut reverse_search);

    assert!(!target.path().join("big").exists());
    assert_eq!(
        fs::read(target.path().join("small/nested/note.txt")).unwrap(),
        b"hello"
    );
    assert!(target.path().join("mid/data.bin").exists());
    assert_eq!(reverse_search.db.total_files(), 3);
    let drive = dupe_rs::volume::drive_letter_of(target.path());
    let label = dupe_rs::volume::volume_info(&drive).label;
    assert!(reverse_search.db.usage(&drive, &label).is_some());

    // Once copied, those folders show up as already present in the target.
    assert_eq!(status_of(&state, "mid"), FolderStatus::ExistsInTarget);

    // Reverse search on the source file finds the copy on the target.
    reverse_search.pick_file(source.path().join("mid/data.bin"));
    wait_until_idle(&mut state, &mut reverse_search);
    assert_eq!(reverse_search.results.len(), 1);
    assert_eq!(
        reverse_search.results[0].absolute_path(),
        target.path().join("mid/data.bin")
    );
}
