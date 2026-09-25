//! Lossless re-encoding of images (and, via ffmpeg, videos) into formats
//! that take less space, keeping a result only when it's actually smaller.

pub mod image_codec;
pub mod video_codec;
pub mod worker;

use crate::control::{ActiveClock, JobControl, estimate_remaining};
use crossbeam_channel::Receiver;
use image_codec::ImageTarget;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use video_codec::VideoMode;
use worker::{FileResult, OriginalHandling, Outcome, ReencodeEvent, ReencodeOptions};

pub struct ReencodeJob {
    rx: Receiver<ReencodeEvent>,
    control: Arc<JobControl>,
    clock: ActiveClock,
    /// `None` until the folder walk has finished.
    pub total_files: Option<usize>,
    pub total_bytes: u64,
    /// Bytes of input fully processed (finished files).
    pub bytes_done: u64,
    pub current: Option<(PathBuf, u64)>,
    /// Fraction of the current file done, for videos.
    pub current_fraction: f32,
}

impl ReencodeJob {
    /// Input bytes processed, counting the finished share of the current file.
    fn progress_bytes(&self) -> u64 {
        let partial = self
            .current
            .as_ref()
            .map_or(0.0, |(_, size)| *size as f64 * self.current_fraction as f64);
        self.bytes_done + partial as u64
    }

    pub fn fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.progress_bytes() as f64 / self.total_bytes as f64) as f32
        }
    }

    pub fn bytes_per_sec(&self) -> Option<f64> {
        let secs = self.clock.elapsed().as_secs_f64();
        let done = self.progress_bytes();
        (done > 0 && secs > 0.0).then(|| done as f64 / secs)
    }

    pub fn eta(&self) -> Option<Duration> {
        estimate_remaining(
            self.progress_bytes(),
            self.total_bytes,
            self.clock.elapsed(),
        )
    }

    pub fn is_paused(&self) -> bool {
        self.control.is_paused()
    }

    pub fn toggle_pause(&mut self) {
        let paused = !self.control.is_paused();
        self.control.set_paused(paused);
        if paused {
            self.clock.pause();
        } else {
            self.clock.resume();
        }
    }

    pub fn cancel(&mut self) {
        self.control.cancel();
        self.clock.resume();
    }

    pub fn is_cancelled(&self) -> bool {
        self.control.is_cancelled()
    }
}

/// Totals across a run's finished files.
#[derive(Default, Clone, Copy)]
pub struct ReencodeTotals {
    pub converted: usize,
    pub skipped: usize,
    pub failed: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

impl ReencodeTotals {
    pub fn saved(&self) -> u64 {
        self.bytes_before.saturating_sub(self.bytes_after)
    }
}

pub struct ReencodeState {
    pub folders: Vec<PathBuf>,
    pub images_enabled: bool,
    pub image_target: ImageTarget,
    pub video_mode: VideoMode,
    pub originals: OriginalHandling,
    pub job: Option<ReencodeJob>,
    pub results: Vec<FileResult>,
    pub totals: ReencodeTotals,
    pub hide_skipped: bool,
    pub status: Option<String>,
    /// Checked lazily the first time videos are enabled.
    ffmpeg: Option<bool>,
}

impl Default for ReencodeState {
    fn default() -> Self {
        Self {
            folders: Vec::new(),
            images_enabled: true,
            image_target: ImageTarget::LosslessWebp,
            video_mode: VideoMode::Skip,
            originals: OriginalHandling::Trash,
            job: None,
            results: Vec::new(),
            totals: ReencodeTotals::default(),
            hide_skipped: true,
            status: None,
            ffmpeg: None,
        }
    }
}

impl ReencodeState {
    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    pub fn ffmpeg_available(&mut self) -> bool {
        *self
            .ffmpeg
            .get_or_insert_with(video_codec::ffmpeg_available)
    }

    pub fn start(&mut self) {
        if self.is_running() || self.folders.is_empty() {
            return;
        }
        let videos = if self.video_mode != VideoMode::Skip && !self.ffmpeg_available() {
            self.status = Some("ffmpeg/ffprobe not found on PATH: videos will be skipped.".into());
            VideoMode::Skip
        } else {
            self.status = None;
            self.video_mode
        };
        let options = ReencodeOptions {
            folders: self.folders.clone(),
            images: self.images_enabled.then_some(self.image_target),
            videos,
            originals: self.originals,
        };
        self.results.clear();
        self.totals = ReencodeTotals::default();

        let (tx, rx) = crossbeam_channel::unbounded();
        let control = Arc::new(JobControl::default());
        let control_for_thread = control.clone();
        std::thread::spawn(move || worker::run(options, tx, &control_for_thread));
        self.job = Some(ReencodeJob {
            rx,
            control,
            clock: ActiveClock::start(),
            total_files: None,
            total_bytes: 0,
            bytes_done: 0,
            current: None,
            current_fraction: 0.0,
        });
    }

    pub fn drain_events(&mut self) -> bool {
        let Some(job) = &mut self.job else {
            return false;
        };
        let mut changed = false;
        let mut finished = false;
        for event in job.rx.try_iter().take(500) {
            changed = true;
            match event {
                ReencodeEvent::Discovered { files, bytes } => {
                    job.total_files = Some(files);
                    job.total_bytes = bytes;
                }
                ReencodeEvent::Started { path, size } => {
                    job.current = Some((path, size));
                    job.current_fraction = 0.0;
                }
                ReencodeEvent::FileProgress(f) => job.current_fraction = f,
                ReencodeEvent::FileDone(result) => {
                    job.bytes_done += result.size;
                    job.current = None;
                    match &result.outcome {
                        Outcome::Saved { new_size, .. } => {
                            self.totals.converted += 1;
                            self.totals.bytes_before += result.size;
                            self.totals.bytes_after += new_size;
                        }
                        Outcome::Skipped(_) => self.totals.skipped += 1,
                        Outcome::Failed(_) => self.totals.failed += 1,
                    }
                    self.results.push(result);
                }
                ReencodeEvent::Finished => finished = true,
            }
        }
        if finished && let Some(job) = self.job.take() {
            let t = self.totals;
            let prefix = if job.is_cancelled() {
                "Cancelled. "
            } else {
                ""
            };
            self.status = Some(format!(
                "{prefix}Re-encoded {} file(s), saving {}; skipped {}, failed {}.",
                t.converted,
                humansize::format_size(t.saved(), humansize::DECIMAL),
                t.skipped,
                t.failed
            ));
        }
        changed
    }
}
