use crate::data::Video;
use chrono::Utc;
use chrono_humanize::{Accuracy, HumanTime, Tense};

const CARD_WIDTH: i32 = 320;
// 16:9 thumbnail height derived from card width
const THUMBNAIL_HEIGHT: i32 = CARD_WIDTH * 9 / 16;
use gdk_pixbuf::Pixbuf;
use gtk::prelude::*;
use gtk::{Align, DrawingArea, EventBox, Label, Orientation};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

/// Widget handles for an individual video card.
#[derive(Clone)]
pub struct VideoCardWidgets {
    root: EventBox,
    watch_later_toggle: gtk::Button,
    summary_button: gtk::Button,
    thumbnail: DrawingArea,
    thumbnail_pixbuf: Rc<RefCell<Option<Pixbuf>>>,
}

impl VideoCardWidgets {
    /// Returns the card root widget.
    pub fn root(&self) -> &EventBox {
        &self.root
    }

    /// Returns the Watch Later toggle button.
    pub fn watch_later_toggle(&self) -> &gtk::Button {
        &self.watch_later_toggle
    }

    /// Returns the AI summary button.
    pub fn summary_button(&self) -> &gtk::Button {
        &self.summary_button
    }

    /// Shows or hides the AI summary button.
    pub fn set_summary_available(&self, has_summary: bool) {
        self.summary_button.set_visible(has_summary);
    }

    /// Reloads the thumbnail image from disk and redraws the card.
    pub fn refresh_thumbnail(&self, thumbnail_path: &Path) {
        *self.thumbnail_pixbuf.borrow_mut() = load_thumbnail(thumbnail_path);
        self.thumbnail.queue_draw();
    }
}

fn load_thumbnail(thumbnail_path: &Path) -> Option<Pixbuf> {
    if !thumbnail_path.exists() {
        return None;
    }

    Pixbuf::from_file(thumbnail_path)
        .ok()
        .map(|pixbuf| crop_to_16_9(&pixbuf))
}

/// Create a video card widget
///
/// # Arguments
///
/// * `video` - Video metadata to display.
/// * `thumbnail_path` - Local thumbnail file path.
/// * `is_watch_later` - Whether the item is already in Watch Later.
/// * `is_downloaded` - Whether the video exists in local cache.
/// * `has_ai_summary` - Whether a cached AI summary exists.
///
/// # Returns
///
/// Widget handles for the created card.
pub fn create_video_card(
    video: &Video,
    thumbnail_path: &Path,
    is_watch_later: bool,
    is_downloaded: bool,
    has_ai_summary: bool,
) -> VideoCardWidgets {
    let event_box = EventBox::new();
    event_box.set_above_child(false);
    event_box.set_hexpand(false);
    event_box.set_halign(Align::Start);

    let card = gtk::Box::new(Orientation::Vertical, 0);
    card.set_widget_name("video-card");
    card.set_size_request(CARD_WIDTH, -1);
    card.set_hexpand(false);
    card.set_halign(Align::Start);

    // Thumbnail - 16:9 aspect ratio, fills card width using DrawingArea
    let thumbnail = DrawingArea::new();
    thumbnail.set_size_request(-1, THUMBNAIL_HEIGHT); // Let width be determined by card
    thumbnail.set_widget_name("thumbnail");
    thumbnail.set_hexpand(true); // Expand to fill card width
    thumbnail.set_vexpand(false);

    // Load the cropped 16:9 pixbuf
    let pixbuf: Rc<RefCell<Option<Pixbuf>>> = Rc::new(RefCell::new(load_thumbnail(thumbnail_path)));

    let pixbuf_for_draw = pixbuf.clone();
    thumbnail.connect_draw(move |widget, cr| {
        let width = widget.allocated_width() as f64;
        let height = widget.allocated_height() as f64;
        let radius = 8.0; // Corner radius matching the card

        // Create clipping path with rounded top corners
        cr.new_path();
        cr.arc(
            radius,
            radius,
            radius,
            std::f64::consts::PI,
            1.5 * std::f64::consts::PI,
        );
        cr.arc(
            width - radius,
            radius,
            radius,
            1.5 * std::f64::consts::PI,
            2.0 * std::f64::consts::PI,
        );
        cr.line_to(width, height);
        cr.line_to(0.0, height);
        cr.close_path();
        cr.clip();

        if let Some(ref pb) = *pixbuf_for_draw.borrow() {
            // Scale uniformly to cover the area (both image and area are 16:9)
            let scale = (width / pb.width() as f64).max(height / pb.height() as f64);
            let scaled_w = pb.width() as f64 * scale;
            let scaled_h = pb.height() as f64 * scale;

            // Center the scaled image
            let x_offset = (width - scaled_w) / 2.0;
            let y_offset = (height - scaled_h) / 2.0;

            cr.translate(x_offset, y_offset);
            cr.scale(scale, scale);
            cr.set_source_pixbuf(pb, 0.0, 0.0);
            cr.paint().ok();
        } else {
            // Draw placeholder background
            cr.set_source_rgb(0.2, 0.2, 0.2);
            cr.rectangle(0.0, 0.0, width, height);
            cr.fill().ok();
        }

        glib::Propagation::Stop
    });

    card.pack_start(&thumbnail, false, false, 0);

    // Content box for text below thumbnail (with padding)
    let content_box = gtk::Box::new(Orientation::Vertical, 4);
    content_box.set_margin_start(8);
    content_box.set_margin_end(8);
    content_box.set_margin_top(8);
    content_box.set_margin_bottom(8);
    content_box.set_hexpand(false);
    content_box.set_size_request(CARD_WIDTH - 16, -1); // card width minus 8px start/end margins

    // Title: wrap naturally and clamp to two lines using Pango layout.
    let title_label = Label::new(Some(video.title()));
    title_label.set_widget_name("video-title");
    title_label.set_line_wrap(true);
    title_label.set_line_wrap_mode(pango::WrapMode::WordChar);
    title_label.set_lines(2);
    title_label.set_ellipsize(pango::EllipsizeMode::End);
    title_label.set_xalign(0.0);
    title_label.set_yalign(0.0);
    // Keep FlowBox card widths stable by constraining the label's natural width.
    // This is a layout hint only; wrapping/ellipsizing still comes from Pango.
    title_label.set_width_chars(36);
    title_label.set_max_width_chars(36);
    content_box.pack_start(&title_label, false, false, 0);

    // Channel name and time
    let meta_box = gtk::Box::new(Orientation::Horizontal, 8);
    meta_box.set_widget_name("video-meta");

    let channel_label = Label::new(Some(video.channel_name()));
    channel_label.set_widget_name("channel-name");
    channel_label.set_ellipsize(pango::EllipsizeMode::End);
    channel_label.set_xalign(0.0);
    channel_label.set_hexpand(true);
    meta_box.pack_start(&channel_label, true, true, 0);

    let time_ago = format_time_ago(video.published());
    let time_and_duration = match video.duration_seconds() {
        Some(seconds) => format!("{} • {}", time_ago, format_video_duration(seconds)),
        None => time_ago,
    };
    let time_label = Label::new(Some(&time_and_duration));
    time_label.set_widget_name("time-ago");
    meta_box.pack_end(&time_label, false, false, 0);

    content_box.pack_start(&meta_box, false, false, 0);

    // Status row with toggle and indicators
    let status_box = gtk::Box::new(Orientation::Horizontal, 4);
    status_box.set_halign(Align::Fill);

    // Watch later toggle button
    let watch_later_toggle = gtk::Button::new();
    set_watch_later_toggle_state(&watch_later_toggle, is_watch_later);
    status_box.pack_start(&watch_later_toggle, false, false, 0);

    // Spacer
    let spacer = gtk::Box::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    status_box.pack_start(&spacer, true, true, 0);

    if is_downloaded {
        let downloaded_badge = gtk::Box::new(Orientation::Horizontal, 0);
        downloaded_badge.set_widget_name("status-downloaded");
        downloaded_badge.set_tooltip_text(Some("Downloaded"));

        let downloaded_icon =
            gtk::Image::from_icon_name(Some("media-floppy-symbolic"), gtk::IconSize::Menu);
        downloaded_badge.pack_start(&downloaded_icon, false, false, 0);

        status_box.pack_end(&downloaded_badge, false, false, 0);
    }

    let summary_button = gtk::Button::with_label("AI");
    summary_button.set_widget_name("status-ai-summary-button");
    summary_button.set_relief(gtk::ReliefStyle::None);
    summary_button.set_can_focus(false);
    summary_button.set_tooltip_text(Some("Show cached AI summary"));
    status_box.pack_end(&summary_button, false, false, 0);
    summary_button.set_visible(has_ai_summary);

    content_box.pack_start(&status_box, false, false, 0);

    card.pack_start(&content_box, false, false, 0);

    event_box.add(&card);
    VideoCardWidgets {
        root: event_box,
        watch_later_toggle,
        summary_button,
        thumbnail,
        thumbnail_pixbuf: pixbuf,
    }
}

/// Update the Watch Later toggle visuals for a card button.
///
/// # Arguments
///
/// * `button` - Toggle button rendered inside a video card.
/// * `is_watch_later` - `true` when the associated video is in Watch Later.
pub fn set_watch_later_toggle_state(button: &gtk::Button, is_watch_later: bool) {
    button.set_widget_name(if is_watch_later {
        "watch-later-toggle-active"
    } else {
        "watch-later-toggle"
    });
    button.set_label(if is_watch_later { "x" } else { "+" });
    button.set_tooltip_text(Some(if is_watch_later {
        "Remove from Watch Later"
    } else {
        "Add to Watch Later"
    }));
}

/// Crop a pixbuf to 16:9 aspect ratio (center crop)
fn crop_to_16_9(pixbuf: &Pixbuf) -> Pixbuf {
    let width = pixbuf.width();
    let height = pixbuf.height();

    // Target 16:9 aspect ratio
    let target_ratio = 16.0 / 9.0;
    let current_ratio = width as f64 / height as f64;

    let (crop_x, crop_y, crop_width, crop_height) = if current_ratio > target_ratio {
        // Image is wider than 16:9, crop sides
        let new_width = (height as f64 * target_ratio) as i32;
        let x_offset = (width - new_width) / 2;
        (x_offset, 0, new_width, height)
    } else {
        // Image is taller than 16:9, crop top/bottom
        let new_height = (width as f64 / target_ratio) as i32;
        let y_offset = (height - new_height) / 2;
        (0, y_offset, width, new_height)
    };

    pixbuf.new_subpixbuf(crop_x, crop_y, crop_width, crop_height)
}

fn format_time_ago(dt: chrono::DateTime<Utc>) -> String {
    HumanTime::from(dt).to_text_en(Accuracy::Rough, Tense::Past)
}

fn format_video_duration(total_seconds: u32) -> String {
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{}:{:02}:{:02}", hours, minutes, seconds)
    } else {
        format!("{}:{:02}", minutes, seconds)
    }
}
