use crossbeam_channel::{Receiver, Sender};
use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const THUMB_SIZE: u32 = 128;
const WORKER_COUNT: usize = 2;

#[derive(Clone)]
pub enum ThumbState {
    Loading,
    Ready(TextureHandle),
    Failed,
    /// Extension isn't a decodable image format — caller should draw a generic icon.
    Unsupported,
}

struct DecodedThumb {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// Lazily decodes and caches thumbnails on background worker threads. Decoding
/// happens off the UI thread; textures are uploaded to the GPU on `poll`,
/// which must run on the UI thread once per frame.
pub struct ThumbnailCache {
    cache: HashMap<PathBuf, ThumbState>,
    inflight: HashSet<PathBuf>,
    request_tx: Sender<PathBuf>,
    result_rx: Receiver<(PathBuf, Option<DecodedThumb>)>,
}

impl ThumbnailCache {
    pub fn new() -> Self {
        let (request_tx, request_rx) = crossbeam_channel::unbounded::<PathBuf>();
        let (result_tx, result_rx) = crossbeam_channel::unbounded();

        for _ in 0..WORKER_COUNT {
            let request_rx = request_rx.clone();
            let result_tx = result_tx.clone();
            std::thread::spawn(move || {
                for path in request_rx {
                    let decoded = decode_thumbnail(&path);
                    if result_tx.send((path, decoded)).is_err() {
                        break;
                    }
                }
            });
        }

        Self {
            cache: HashMap::new(),
            inflight: HashSet::new(),
            request_tx,
            result_rx,
        }
    }

    /// Uploads any freshly decoded thumbnails as GPU textures. Call once per frame.
    pub fn poll(&mut self, ctx: &Context) {
        for (path, decoded) in self.result_rx.try_iter().take(64) {
            self.inflight.remove(&path);
            let state = match decoded {
                Some(d) => {
                    let image = ColorImage::from_rgba_unmultiplied(
                        [d.width as usize, d.height as usize],
                        &d.rgba,
                    );
                    let handle =
                        ctx.load_texture(path.to_string_lossy(), image, TextureOptions::LINEAR);
                    ThumbState::Ready(handle)
                }
                None => ThumbState::Failed,
            };
            self.cache.insert(path, state);
        }
    }

    /// Returns the current thumbnail state for `path`, kicking off an async
    /// decode on first request. Only called for entries actually drawn on
    /// screen, so scrolling a grid never queues more than what's visible.
    pub fn get_or_request(&mut self, path: &Path) -> ThumbState {
        if let Some(state) = self.cache.get(path) {
            return state.clone();
        }

        let is_image = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(image::ImageFormat::from_extension)
            .is_some();

        if !is_image {
            self.cache
                .insert(path.to_path_buf(), ThumbState::Unsupported);
            return ThumbState::Unsupported;
        }

        if self.inflight.insert(path.to_path_buf()) {
            let _ = self.request_tx.send(path.to_path_buf());
        }
        self.cache.insert(path.to_path_buf(), ThumbState::Loading);
        ThumbState::Loading
    }
}

impl Default for ThumbnailCache {
    fn default() -> Self {
        Self::new()
    }
}

fn decode_thumbnail(path: &Path) -> Option<DecodedThumb> {
    let img = image::open(path).ok()?;
    let thumb = img.thumbnail(THUMB_SIZE, THUMB_SIZE).to_rgba8();
    let (width, height) = thumb.dimensions();
    Some(DecodedThumb {
        width,
        height,
        rgba: thumb.into_raw(),
    })
}
