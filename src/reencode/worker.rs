use super::image_codec::{self, ImageTarget};
use super::video_codec::{self, VideoMode};
use crate::control::JobControl;
use crossbeam_channel::Sender;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// What happens to an original once a smaller re-encode exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginalHandling {
    /// Move the original to the Recycle Bin; the new file takes its place.
    Trash,
    /// Keep the original and write the new file next to it.
    Keep,
}

#[derive(Clone, Debug)]
pub struct ReencodeOptions {
    pub folders: Vec<PathBuf>,
    pub images: Option<ImageTarget>,
    pub videos: VideoMode,
    pub originals: OriginalHandling,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Replaced/added a smaller file at `new_path`.
    Saved {
        new_path: PathBuf,
        new_size: u64,
    },
    Skipped(String),
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct FileResult {
    pub path: PathBuf,
    pub size: u64,
    pub outcome: Outcome,
}

pub enum ReencodeEvent {
    /// Candidates found; sent once before any encoding starts.
    Discovered {
        files: usize,
        bytes: u64,
    },
    Started {
        path: PathBuf,
        size: u64,
    },
    /// Fraction of the current (video) file done.
    FileProgress(f32),
    FileDone(FileResult),
    Finished,
}

/// Walks `options.folders` for re-encodable media, then processes each file
/// in turn: encode to a hidden temp file beside it, keep the result only if
/// it's smaller, carry the timestamps over, and swap it in (see
/// `OriginalHandling`). Nothing is replaced until the new file is complete,
/// verified (images are compared pixel by pixel) and on disk.
pub fn run(options: ReencodeOptions, tx: Sender<ReencodeEvent>, control: &JobControl) {
    let mut files = Vec::new();
    for folder in &options.folders {
        for entry in WalkDir::new(folder)
            .follow_links(false)
            .into_iter()
            .flatten()
        {
            if control.checkpoint() {
                let _ = tx.send(ReencodeEvent::Finished);
                return;
            }
            let path = entry.path();
            let wanted = entry.file_type().is_file()
                && !is_temp(path)
                && ((options.images.is_some() && image_codec::is_reencodable_image(path))
                    || (options.videos != VideoMode::Skip && video_codec::is_video(path)));
            if wanted && let Ok(meta) = entry.metadata() {
                files.push((path.to_path_buf(), meta.len()));
            }
        }
    }
    let _ = tx.send(ReencodeEvent::Discovered {
        files: files.len(),
        bytes: files.iter().map(|(_, s)| s).sum(),
    });

    for (path, size) in files {
        if control.checkpoint() {
            break;
        }
        let _ = tx.send(ReencodeEvent::Started {
            path: path.clone(),
            size,
        });
        let Some(outcome) = process_file(&path, size, &options, control, &tx) else {
            break; // cancelled mid-file
        };
        let _ = tx.send(ReencodeEvent::FileDone(FileResult {
            path,
            size,
            outcome,
        }));
    }
    let _ = tx.send(ReencodeEvent::Finished);
}

const TEMP_MARKER: &str = ".dupe-rs-tmp";

fn is_temp(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains(TEMP_MARKER))
}

/// `None` if cancelled.
fn process_file(
    path: &Path,
    size: u64,
    options: &ReencodeOptions,
    control: &JobControl,
    tx: &Sender<ReencodeEvent>,
) -> Option<Outcome> {
    let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
        return Some(Outcome::Skipped("no file name".into()));
    };
    let dir = path.parent().unwrap_or(Path::new("."));

    // Encode into a temp file next to the original.
    let (temp, ext) = if image_codec::is_reencodable_image(path) {
        let Some(target) = options.images else {
            return Some(Outcome::Skipped("images are skipped".into()));
        };
        let (bytes, ext) = match image_codec::reencode_image(path, target) {
            Ok(r) => r,
            Err(e) => return Some(Outcome::Skipped(e)),
        };
        if bytes.len() as u64 >= size {
            return Some(Outcome::Skipped("already as small as it gets".into()));
        }
        let temp = dir.join(format!(".{stem}{TEMP_MARKER}.{ext}"));
        if let Err(e) = std::fs::write(&temp, &bytes) {
            let _ = std::fs::remove_file(&temp);
            return Some(Outcome::Failed(e.to_string()));
        }
        (temp, ext)
    } else {
        let prepared = match video_codec::prepare(path, options.videos) {
            Ok(p) => p,
            Err(e) => return Some(Outcome::Skipped(e)),
        };
        let temp = dir.join(format!(".{stem}{TEMP_MARKER}.{}", prepared.ext));
        match video_codec::reencode_video(path, &temp, &prepared, control, |f| {
            let _ = tx.send(ReencodeEvent::FileProgress(f));
        }) {
            Ok(true) => {}
            Ok(false) => return None,
            Err(e) => return Some(Outcome::Failed(e)),
        }
        (temp, prepared.ext)
    };

    Some(finalize(path, size, &stem, ext, &temp, options.originals))
}

/// Swaps the finished temp file in, or discards it if it isn't a saving.
fn finalize(
    path: &Path,
    size: u64,
    stem: &str,
    ext: &str,
    temp: &Path,
    originals: OriginalHandling,
) -> Outcome {
    let discard = |outcome: Outcome| {
        let _ = std::fs::remove_file(temp);
        outcome
    };
    let new_size = match std::fs::metadata(temp) {
        Ok(m) => m.len(),
        Err(e) => return discard(Outcome::Failed(e.to_string())),
    };
    if new_size >= size {
        return discard(Outcome::Skipped("re-encoded file wasn't smaller".into()));
    }

    let dir = path.parent().unwrap_or(Path::new("."));
    let same_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext));
    let new_path = match (originals, same_ext) {
        (OriginalHandling::Trash, true) => path.to_path_buf(),
        (OriginalHandling::Keep, true) => dir.join(format!("{stem} (re-encoded).{ext}")),
        (_, false) => dir.join(format!("{stem}.{ext}")),
    };
    if new_path != path && new_path.exists() {
        return discard(Outcome::Skipped(format!(
            "{} already exists",
            new_path.file_name().unwrap_or_default().to_string_lossy()
        )));
    }

    if let Ok(meta) = std::fs::metadata(path) {
        copy_times(&meta, temp);
    }
    if originals == OriginalHandling::Trash
        && let Err(e) = trash::delete(path)
    {
        return discard(Outcome::Failed(format!(
            "couldn't move the original to the trash: {e}"
        )));
    }
    if let Err(e) = std::fs::rename(temp, &new_path) {
        // The original may already be in the trash; leave the temp file in
        // place rather than lose both copies.
        return Outcome::Failed(format!(
            "couldn't move the new file into place ({e}); it was left at {}",
            temp.display()
        ));
    }
    Outcome::Saved { new_path, new_size }
}

fn copy_times(meta: &std::fs::Metadata, to: &Path) {
    let Ok(file) = std::fs::OpenOptions::new().write(true).open(to) else {
        return;
    };
    let mut times = std::fs::FileTimes::new();
    if let Ok(modified) = meta.modified() {
        times = times.set_modified(modified);
    }
    #[cfg(windows)]
    if let Ok(created) = meta.created() {
        use std::os::windows::fs::FileTimesExt;
        times = times.set_created(created);
    }
    let _ = file.set_times(times);
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};
    use std::fs;

    fn write_bmp(path: &Path) {
        ImageBuffer::from_fn(64, 48, |x, y| Rgb([(x * 4) as u8, (y * 5) as u8, 80]))
            .save(path)
            .unwrap();
    }

    fn run_on(dir: &Path, originals: OriginalHandling) -> Vec<FileResult> {
        let (tx, rx) = crossbeam_channel::unbounded();
        run(
            ReencodeOptions {
                folders: vec![dir.to_path_buf()],
                images: Some(ImageTarget::LosslessWebp),
                videos: VideoMode::Skip,
                originals,
            },
            tx,
            &JobControl::default(),
        );
        rx.try_iter()
            .filter_map(|e| match e {
                ReencodeEvent::FileDone(r) => Some(r),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn keeps_the_original_and_writes_a_smaller_lossless_copy() {
        let dir = tempfile::tempdir().unwrap();
        let bmp = dir.path().join("pic.bmp");
        write_bmp(&bmp);
        fs::write(dir.path().join("photo.jpg"), b"not touched").unwrap();

        let results = run_on(dir.path(), OriginalHandling::Keep);

        assert_eq!(results.len(), 1, "only the BMP is a candidate");
        let webp = dir.path().join("pic.webp");
        assert!(
            matches!(&results[0].outcome, Outcome::Saved { new_path, .. } if *new_path == webp)
        );
        assert!(bmp.exists());
        assert_eq!(
            image::open(&webp).unwrap().to_rgb8(),
            image::open(&bmp).unwrap().to_rgb8()
        );
        assert_eq!(
            fs::metadata(&webp).unwrap().modified().unwrap(),
            fs::metadata(&bmp).unwrap().modified().unwrap()
        );
        // No temp files left behind.
        assert!(
            fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .all(|e| !is_temp(&e.path()))
        );
    }

    #[test]
    fn an_existing_file_with_the_new_name_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        write_bmp(&dir.path().join("pic.bmp"));
        fs::write(dir.path().join("pic.webp"), b"someone else's file").unwrap();

        let results = run_on(dir.path(), OriginalHandling::Keep);

        assert!(matches!(results[0].outcome, Outcome::Skipped(_)));
        assert_eq!(
            fs::read(dir.path().join("pic.webp")).unwrap(),
            b"someone else's file"
        );
    }
}
