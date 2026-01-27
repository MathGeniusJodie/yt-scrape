use crate::data::Video;
use crate::urls;
use anyhow::Result;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Manages thumbnail downloads and chafa rendering
pub struct ThumbnailCache {
    cache_dir: PathBuf,
    /// In-memory cache of rendered thumbnails (video_id_WxH -> ANSI string)
    rendered: HashMap<String, (String, Vec<(u8, u8, u8)>)>,
}

impl ThumbnailCache {
    pub fn new(cache_dir: PathBuf) -> Result<Self> {
        let thumb_dir = cache_dir.join("thumbnails");
        std::fs::create_dir_all(&thumb_dir)?;

        Ok(Self {
            cache_dir,
            rendered: HashMap::new(),
        })
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

    /// Get rendered thumbnail from cache, or render it if needed
    pub fn get_rendered(
        &mut self,
        video_id: &str,
        width: u16,
        height: u16,
    ) -> Option<(String, Vec<(u8, u8, u8)>)> {
        let cache_key = format!("{}_{}x{}", video_id, width, height);

        // Check memory cache
        if let Some(rendered) = self.rendered.get(&cache_key) {
            return Some(rendered.clone());
        }

        // Check if raw thumbnail exists
        let thumb_path = self.thumbnail_path(video_id);
        if !thumb_path.exists() {
            return None;
        }

        // Render with chafa
        if let Some(rendered) = self.render_with_chafa(&thumb_path, width, height) {
            self.rendered.insert(cache_key, rendered.clone());
            return Some(rendered);
        }

        None
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

    /// Render an image with chafa, cropping to 16:9 first
    fn render_with_chafa(
        &self,
        path: &Path,
        width: u16,
        height: u16,
    ) -> Option<(String, Vec<(u8, u8, u8)>)> {
        use std::process::Stdio;

        // Use ImageMagick to crop to 16:9 centered, then pipe to chafa
        let convert = Command::new("convert")
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

        let bytes = Command::new("convert")
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
    pub fn clear_rendered_cache(&mut self) {
        self.rendered.clear();
    }
}
