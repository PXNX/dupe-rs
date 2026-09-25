use crate::control::JobControl;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// How videos are treated by a re-encode run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoMode {
    /// Leave videos alone.
    Skip,
    /// Only videos stored in a lossless/uncompressed codec (raw, HuffYUV,
    /// UtVideo, ...), re-encoded to FFV1: bit-exact and much smaller.
    /// Already-compressed video is skipped, since losslessly re-encoding it
    /// would make it bigger.
    LosslessOnly,
    /// Re-encode everything not already in a modern codec to H.265 at a
    /// quality indistinguishable to the eye. Not bit-exact.
    VisuallyLossless,
}

impl VideoMode {
    pub fn label(self) -> &'static str {
        match self {
            VideoMode::Skip => "Skip videos",
            VideoMode::LosslessOnly => "Lossless only (FFV1)",
            VideoMode::VisuallyLossless => "Visually lossless (H.265, not bit-exact)",
        }
    }
}

pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "webm", "m4v", "wmv", "flv", "mpg", "mpeg",
];

/// Video codecs that store frames losslessly; re-encoding them to FFV1
/// keeps every pixel while typically saving a lot of space.
const LOSSLESS_SOURCE_CODECS: &[&str] = &[
    "rawvideo", "huffyuv", "ffvhuff", "utvideo", "magicyuv", "r210", "v210", "v410", "png",
    "qtrle", "zlib", "lagarith", "rpza", "bmp", "tiff",
];

/// Codecs already efficient enough that another lossy pass isn't worth it.
const MODERN_CODECS: &[&str] = &["hevc", "av1", "vp9", "ffv1"];

pub fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.to_lowercase().as_str()))
}

/// A `Command` that doesn't flash a console window from the GUI build.
fn quiet_command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.stdin(Stdio::null());
    cmd
}

pub fn ffmpeg_available() -> bool {
    ["ffmpeg", "ffprobe"].iter().all(|tool| {
        quiet_command(tool)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

struct Probe {
    codec: String,
    duration: Option<Duration>,
}

fn probe(path: &Path) -> Result<Probe, String> {
    let out = quiet_command("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=codec_name:format=duration"])
        .args(["-of", "default=noprint_wrappers=1"])
        .arg(path)
        .output()
        .map_err(|e| format!("couldn't run ffprobe: {e}"))?;
    if !out.status.success() {
        return Err("ffprobe couldn't read it".into());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key).map(|v| v.trim().to_string()))
    };
    let codec = field("codec_name=").ok_or("no video stream")?;
    let duration = field("duration=")
        .and_then(|d| d.parse::<f64>().ok())
        .filter(|d| d.is_finite() && *d > 0.0)
        .map(Duration::from_secs_f64);
    Ok(Probe { codec, duration })
}

/// What to do with one video under `mode`: the ffmpeg arguments and output
/// extension, or why it's skipped.
fn plan(
    path: &Path,
    codec: &str,
    mode: VideoMode,
) -> Result<(Vec<&'static str>, &'static str), String> {
    match mode {
        VideoMode::Skip => Err("videos are skipped".into()),
        VideoMode::LosslessOnly if LOSSLESS_SOURCE_CODECS.contains(&codec) => Ok((
            vec![
                "-map",
                "0",
                "-c",
                "copy",
                "-c:v",
                "ffv1",
                "-level",
                "3",
                "-g",
                "1",
                "-slicecrc",
                "1",
                "-c:a",
                "flac",
            ],
            "mkv",
        )),
        VideoMode::LosslessOnly => Err(format!(
            "already compressed ({codec}); a lossless re-encode would be larger"
        )),
        VideoMode::VisuallyLossless if MODERN_CODECS.contains(&codec) => {
            Err(format!("already in an efficient codec ({codec})"))
        }
        VideoMode::VisuallyLossless => {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_lowercase);
            let mp4 = matches!(ext.as_deref(), Some("mp4" | "m4v" | "mov"));
            Ok((
                vec![
                    "-map", "0:v:0", "-map", "0:a?", "-c:v", "libx265", "-crf", "20", "-preset",
                    "medium", "-tag:v", "hvc1", "-c:a", "copy",
                ],
                if mp4 { "mp4" } else { "mkv" },
            ))
        }
    }
}

/// A video that `mode` will re-encode: what ffmpeg is told and the output
/// extension.
pub struct PreparedVideo {
    args: Vec<&'static str>,
    pub ext: &'static str,
    duration: Option<Duration>,
}

/// Probes `path` and decides what `mode` does with it, or why it's skipped.
pub fn prepare(path: &Path, mode: VideoMode) -> Result<PreparedVideo, String> {
    if mode == VideoMode::Skip {
        return Err("videos are skipped".into());
    }
    let probe = probe(path)?;
    let (args, ext) = plan(path, &probe.codec, mode)?;
    Ok(PreparedVideo {
        args,
        ext,
        duration: probe.duration,
    })
}

/// Encodes `src` into `dst` as `prepared` says. `on_progress` gets the
/// fraction of the video done. Returns `Ok(false)` if cancelled (and removes
/// `dst`). Pausing takes effect before the next file, since a running
/// ffmpeg isn't suspended.
pub fn reencode_video(
    src: &Path,
    dst: &Path,
    prepared: &PreparedVideo,
    control: &JobControl,
    mut on_progress: impl FnMut(f32),
) -> Result<bool, String> {
    let args = &prepared.args;
    let mut child = quiet_command("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-i"])
        .arg(src)
        .args(args)
        .args(["-progress", "pipe:1", "-nostats"])
        .arg(dst)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't run ffmpeg: {e}"))?;

    // ffmpeg reports `out_time_us=<microseconds>` lines on stdout.
    let (tx, rx) = crossbeam_channel::unbounded();
    let stdout = child.stdout.take().expect("piped");
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(us) = line
                .strip_prefix("out_time_us=")
                .and_then(|v| v.parse::<u64>().ok())
            {
                let _ = tx.send(Duration::from_micros(us));
            }
        }
    });
    let stderr = child.stderr.take().expect("piped");
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut BufReader::new(stderr), &mut text);
        text
    });

    let status = loop {
        if control.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(dst);
            return Ok(false);
        }
        for done in rx.try_iter() {
            if let Some(total) = prepared.duration {
                on_progress((done.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0));
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(e.to_string()),
        }
    };
    if !status.success() {
        let _ = std::fs::remove_file(dst);
        let text = errors.join().unwrap_or_default();
        let first = text
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("ffmpeg failed");
        return Err(first.to_string());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossless_mode_only_accepts_lossless_sources() {
        let p = Path::new("clip.avi");
        assert_eq!(
            plan(p, "rawvideo", VideoMode::LosslessOnly).unwrap().1,
            "mkv"
        );
        assert!(plan(p, "h264", VideoMode::LosslessOnly).is_err());
    }

    #[test]
    fn visually_lossless_keeps_mp4_containers_and_skips_modern_codecs() {
        assert_eq!(
            plan(Path::new("a.MP4"), "h264", VideoMode::VisuallyLossless)
                .unwrap()
                .1,
            "mp4"
        );
        assert_eq!(
            plan(Path::new("a.avi"), "mpeg4", VideoMode::VisuallyLossless)
                .unwrap()
                .1,
            "mkv"
        );
        assert!(plan(Path::new("a.mkv"), "hevc", VideoMode::VisuallyLossless).is_err());
    }
}
