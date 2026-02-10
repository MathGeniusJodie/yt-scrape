use crate::data::Video;
use chrono::Utc;
use gdk_pixbuf::Pixbuf;
use gtk::prelude::*;
use gtk::{Align, DrawingArea, EventBox, Label, Orientation};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

/// Create a video card widget
/// Returns the card EventBox and the watch later toggle button
pub fn create_video_card(
    video: &Video,
    thumbnail_path: &Path,
    is_watch_later: bool,
    is_downloaded: bool,
) -> (EventBox, gtk::Button) {
    let event_box = EventBox::new();
    event_box.set_above_child(false);
    event_box.set_hexpand(false);
    event_box.set_halign(Align::Start);

    let card = gtk::Box::new(Orientation::Vertical, 0);
    card.set_widget_name("video-card");
    card.set_size_request(320, -1);
    card.set_hexpand(false);
    card.set_halign(Align::Start);

    // Thumbnail - 16:9 aspect ratio, fills card width using DrawingArea
    let thumbnail = DrawingArea::new();
    thumbnail.set_size_request(-1, 180); // Let width be determined by card
    thumbnail.set_widget_name("thumbnail");
    thumbnail.set_hexpand(true); // Expand to fill card width
    thumbnail.set_vexpand(false);

    // Load the cropped 16:9 pixbuf
    let pixbuf: Rc<RefCell<Option<Pixbuf>>> = Rc::new(RefCell::new(None));
    if thumbnail_path.exists() {
        if let Ok(pb) = Pixbuf::from_file(thumbnail_path) {
            let cropped = crop_to_16_9(&pb);
            *pixbuf.borrow_mut() = Some(cropped);
        }
    }

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
    content_box.set_size_request(304, -1); // 320 - 16px margins

    // Title - always 2 lines
    let title_text = format_two_line_title(&video.title);
    let title_label = Label::new(Some(&title_text));
    title_label.set_widget_name("video-title");
    title_label.set_line_wrap(true);
    title_label.set_line_wrap_mode(pango::WrapMode::WordChar);
    title_label.set_lines(2);
    title_label.set_ellipsize(pango::EllipsizeMode::End);
    title_label.set_xalign(0.0);
    title_label.set_width_chars(36);
    title_label.set_max_width_chars(36);
    content_box.pack_start(&title_label, false, false, 0);

    // Channel name and time
    let meta_box = gtk::Box::new(Orientation::Horizontal, 8);
    meta_box.set_widget_name("video-meta");

    let channel_label = Label::new(Some(&video.channel_name));
    channel_label.set_widget_name("channel-name");
    channel_label.set_ellipsize(pango::EllipsizeMode::End);
    channel_label.set_xalign(0.0);
    channel_label.set_hexpand(true);
    meta_box.pack_start(&channel_label, true, true, 0);

    let time_ago = format_time_ago(&video.published);
    let time_label = Label::new(Some(&time_ago));
    time_label.set_widget_name("time-ago");
    meta_box.pack_end(&time_label, false, false, 0);

    content_box.pack_start(&meta_box, false, false, 0);

    // Status row with toggle and indicators
    let status_box = gtk::Box::new(Orientation::Horizontal, 4);
    status_box.set_halign(Align::Fill);

    // Watch later toggle button
    let watch_later_toggle = gtk::Button::new();
    watch_later_toggle.set_widget_name(if is_watch_later {
        "watch-later-toggle-active"
    } else {
        "watch-later-toggle"
    });
    watch_later_toggle.set_label(if is_watch_later { "✓" } else { "+" });
    watch_later_toggle.set_tooltip_text(Some(if is_watch_later {
        "Remove from Watch Later"
    } else {
        "Add to Watch Later"
    }));
    status_box.pack_start(&watch_later_toggle, false, false, 0);

    // Spacer
    let spacer = gtk::Box::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    status_box.pack_start(&spacer, true, true, 0);

    if is_downloaded {
        let downloaded_label = Label::new(Some("Downloaded"));
        downloaded_label.set_widget_name("status-downloaded");
        status_box.pack_end(&downloaded_label, false, false, 0);
    }

    content_box.pack_start(&status_box, false, false, 0);

    card.pack_start(&content_box, false, false, 0);

    event_box.add(&card);
    (event_box, watch_later_toggle)
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

/// Format title to always occupy 2 lines
/// - Short titles get an em dash on the second line
/// - Long titles wrap and get ellipsized if > 2 lines
fn format_two_line_title(title: &str) -> String {
    // Approximate characters that fit on one line at 320px width
    const CHARS_PER_LINE: usize = 38;

    if title.chars().count() <= CHARS_PER_LINE {
        // Short title - add em dash on second line
        format!("{}\n—", title)
    } else {
        // Long title - let it wrap naturally (will be ellipsized if > 2 lines)
        title.to_string()
    }
}

fn format_time_ago(dt: &chrono::DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(*dt);

    if duration.num_days() > 365 {
        let years = duration.num_days() / 365;
        format!("{}y ago", years)
    } else if duration.num_days() > 30 {
        let months = duration.num_days() / 30;
        format!("{}mo ago", months)
    } else if duration.num_days() > 0 {
        format!("{}d ago", duration.num_days())
    } else if duration.num_hours() > 0 {
        format!("{}h ago", duration.num_hours())
    } else if duration.num_minutes() > 0 {
        format!("{}m ago", duration.num_minutes())
    } else {
        "just now".to_string()
    }
}
