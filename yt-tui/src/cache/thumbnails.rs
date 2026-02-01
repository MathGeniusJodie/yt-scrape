use crate::data::Video;
use crate::urls;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Manages thumbnail downloads and chafa rendering
pub struct ThumbnailCache {
    cache_dir: PathBuf,
    /// In-memory cache of rendered thumbnails (video_id_WxH -> ANSI string)
    rendered: Arc<Mutex<HashMap<String, (String, Vec<(u8, u8, u8)>)>>>,
    /// Set of cache keys currently being rendered (to avoid duplicate work)
    pending: Arc<Mutex<HashSet<String>>>,
    /// Channel to notify when a thumbnail finishes rendering
    render_complete_tx: mpsc::UnboundedSender<()>,
}

impl ThumbnailCache {
    pub fn new(cache_dir: PathBuf) -> Result<(Self, mpsc::UnboundedReceiver<()>)> {
        let thumb_dir = cache_dir.join("thumbnails");
        std::fs::create_dir_all(&thumb_dir)?;

        let (tx, rx) = mpsc::unbounded_channel();

        Ok((
            Self {
                cache_dir,
                rendered: Arc::new(Mutex::new(HashMap::new())),
                pending: Arc::new(Mutex::new(HashSet::new())),
                render_complete_tx: tx,
            },
            rx,
        ))
    }

    /// Get the path where a thumbnail would be stored
    pub fn thumbnail_path(&self, video_id: &str) -> PathBuf {
        self.cache_dir
            .join("thumbnails")
            .join(format!("{}.jpg", video_id))
    }

    /// Check if thumbnail exists on disk
    pub fn has_thumbnail(&self, video_id: &str) -> bool {
        self.thumbnail_path(video_id).exists()
    }

    /// Get rendered thumbnail from cache (non-blocking, returns None if not cached)
    pub fn get_rendered(
        &self,
        video_id: &str,
        width: u16,
        height: u16,
    ) -> Option<(String, Vec<(u8, u8, u8)>)> {
        let cache_key = format!("{}_{}x{}", video_id, width, height);

        // Only check memory cache - never block
        let cache = self.rendered.lock().ok()?;
        cache.get(&cache_key).cloned()
    }

    /// Queue a thumbnail for background rendering if not already cached or pending
    pub fn queue_render(&self, video_id: &str, width: u16, height: u16) {
        let cache_key = format!("{}_{}x{}", video_id, width, height);

        // Check if already cached
        if let Ok(cache) = self.rendered.lock() {
            if cache.contains_key(&cache_key) {
                return;
            }
        }

        // Check if already pending
        {
            let mut pending = match self.pending.lock() {
                Ok(p) => p,
                Err(_) => return,
            };
            if pending.contains(&cache_key) {
                return;
            }
            pending.insert(cache_key.clone());
        }

        // Check if raw thumbnail exists
        let thumb_path = self.thumbnail_path(video_id);
        if !thumb_path.exists() {
            // Remove from pending since we can't render it
            if let Ok(mut pending) = self.pending.lock() {
                pending.remove(&cache_key);
            }
            return;
        }

        // Spawn blocking task to render
        let rendered_cache = Arc::clone(&self.rendered);
        let pending_set = Arc::clone(&self.pending);
        let cache_key_clone = cache_key.clone();
        let notify_tx = self.render_complete_tx.clone();

        tokio::task::spawn_blocking(move || {
            let success =
                if let Some(result) = Self::render_with_chafa_static(&thumb_path, width, height) {
                    if let Ok(mut cache) = rendered_cache.lock() {
                        cache.insert(cache_key_clone.clone(), result);
                    }
                    true
                } else {
                    false
                };
            // Remove from pending
            if let Ok(mut pending) = pending_set.lock() {
                pending.remove(&cache_key_clone);
            }
            // Notify that a render completed
            if success {
                let _ = notify_tx.send(());
            }
        });
    }

    /// Download a thumbnail from YouTube
    pub async fn download(&self, video: &Video) -> Result<()> {
        let thumb_path = self.thumbnail_path(&video.video_id);

        if thumb_path.exists() {
            return Ok(());
        }

        let url = urls::thumbnail_url(&video.video_id);

        let response = reqwest::get(&url).await?;
        let bytes = response.bytes().await?;
        tokio::fs::write(&thumb_path, &bytes).await?;

        Ok(())
    }

    /// Render an image with chafa, cropping to 16:9 first (static version for spawn_blocking)
    fn render_with_chafa_static(
        path: &Path,
        width: u16,
        height: u16,
    ) -> Option<(String, Vec<(u8, u8, u8)>)> {
        use std::process::Stdio;

        // Use ImageMagick to crop to 16:9 centered, then pipe to chafa
        let convert = Command::new("magick")
            .args([
                path.to_str()?,
                "-gravity",
                "center",
                "-crop",
                "16:9",
                "+repage",
                "png:-",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;

        let bytes = Command::new("magick")
            .args(&[
                path.to_str()?,
                "-gravity",
                "center",
                "-crop",
                "16:9",
                "+repage",
                "-resize",
                &format!("{}x{}!", width, height as u32 * 2),
                "-depth",
                "8",
                "-colorspace",
                "RGB",
                "RGB:-",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .ok()?
            .stdout?
            .bytes();
        let mut vec = Vec::with_capacity((width * height * 2 * 3).into());

        let curve = |v: u8| -> u8 {
            let x = v as f32;
            (x * (0.625 - 0.0012207 * x)).round() as u8
        };

        for chunk in bytes
            .into_iter()
            .collect::<Result<Vec<u8>, _>>()
            .ok()?
            .chunks(3)
        {
            vec.push((curve(chunk[0]), curve(chunk[1]), curve(chunk[2])));
        }
        // get last width*4 tuples
        // the 4 is for 4 rows, not channels
        let fancy_bg = vec
            .into_iter()
            .rev()
            .take((width as usize) * 4)
            .collect::<Vec<(u8, u8, u8)>>()
            .into_iter()
            .rev()
            .collect::<Vec<(u8, u8, u8)>>();

        let output = Command::new("chafa")
            .args([
                "--size",
                &format!("{}x{}", width, height),
                "--symbols",
                "sextant+block+quad",
                "--work",
                "9",
                "-",
            ])
            .stdin(convert.stdout?)
            .output()
            .ok()?;

        let success = output.status.success();
        // remove last line from image
        let output = String::from_utf8(output.stdout)
            .ok()?
            .lines()
            .take(height.saturating_sub(2) as usize)
            .collect::<Vec<&str>>()
            .join("\n");

        if success {
            Some((output, fancy_bg))
        } else {
            None
        }
    }

    /// Clear memory cache (useful when terminal resizes)
    pub fn clear_rendered_cache(&self) {
        if let Ok(mut cache) = self.rendered.lock() {
            cache.clear();
        }
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
    }
}
