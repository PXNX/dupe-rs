use crossbeam_channel::Receiver;
use std::path::PathBuf;

/// What a file/folder picker was opened for, and so where its result goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickPurpose {
    ScanFolders,
    IndexFolders,
    SearchFile,
    FillSource,
    FillTarget,
    ReencodeFolders,
    FlattenRoot,
}

impl PickPurpose {
    fn open(self) -> Vec<PathBuf> {
        let dialog = rfd::FileDialog::new();
        match self {
            PickPurpose::ScanFolders | PickPurpose::IndexFolders | PickPurpose::ReencodeFolders => {
                dialog.pick_folders().unwrap_or_default()
            }
            PickPurpose::FillSource | PickPurpose::FillTarget | PickPurpose::FlattenRoot => {
                dialog.pick_folder().into_iter().collect()
            }
            PickPurpose::SearchFile => dialog.pick_file().into_iter().collect(),
        }
    }
}

/// A native picker running on its own thread. The Windows shell dialog
/// enumerates drives before it even appears, which can take a long time
/// when a drive is busy or asleep; opened on the UI thread, that froze the
/// whole window ("Not responding") until it showed up.
pub struct PendingPick {
    pub purpose: PickPurpose,
    rx: Receiver<Vec<PathBuf>>,
}

impl PendingPick {
    pub fn open(purpose: PickPurpose) -> Self {
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send(purpose.open());
        });
        Self { purpose, rx }
    }

    /// The chosen paths (empty if the dialog was cancelled) once it closes.
    pub fn poll(&self) -> Option<Vec<PathBuf>> {
        self.rx.try_recv().ok()
    }
}
