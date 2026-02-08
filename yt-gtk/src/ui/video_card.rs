use crate::data::Video;
use chrono::Utc;
use gdk_pixbuf::Pixbuf;
use gtk::prelude::*;
use gtk::{Align, EventBox, Image, Label, Orientation};
use std::path::Path;

/// Create a video card widget
pub fn create_video_card(
    video: &Video,
    thumbnail_path: &Path,
    is_watch_later: bool,
    is_downloaded: bool,
) -> EventBox {
    let event_box = EventBox::new();
    event_box.set_above_child(false);
    event_box.set_hexpand(false);
    event_box.set_halign(Align::Start);

    let card = gtk::Box::new(Orientation::Vertical, 4);
    card.set_widget_name("video-card");
    card.set_size_request(320, -1);
    card.set_hexpand(false);
    card.set_margin_start(8);
    card.set_margin_end(8);
    card.set_margin_top(8);
    card.set_margin_bottom(8);

    // Thumbnail - 16:9 aspect ratio (320x180)
    let thumbnail = if thumbnail_path.exists() {
        // Load and crop to 16:9 (hqdefault is 480x360 = 4:3 with letterboxing)
        match Pixbuf::from_file(thumbnail_path) {
            Ok(pixbuf) => {
                let cropped = crop_to_16_9(&pixbuf);
                let scaled = cropped.scale_simple(320, 180, gdk_pixbuf::InterpType::Bilinear)
                    .unwrap_or(cropped);
                Image::from_pixbuf(Some(&scaled))
            }
            Err(_) => create_placeholder_image(),
        }
    } else {
        create_placeholder_image()
    };
    thumbnail.set_size_request(320, 180);
    thumbnail.set_widget_name("thumbnail");
    card.pack_start(&thumbnail, false, false, 0);

    // Title
    let title_label = Label::new(Some(&video.title));
    title_label.set_widget_name("video-title");
    title_label.set_line_wrap(true);
    title_label.set_line_wrap_mode(pango::WrapMode::WordChar);
    title_label.set_lines(2);
    title_label.set_ellipsize(pango::EllipsizeMode::End);
    title_label.set_xalign(0.0);
    title_label.set_max_width_chars(40);
    card.pack_start(&title_label, false, false, 0);

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

    card.pack_start(&meta_box, false, false, 0);

    // Status indicators
    let status_box = gtk::Box::new(Orientation::Horizontal, 4);
    status_box.set_halign(Align::End);

    if is_downloaded {
        let downloaded_label = Label::new(Some("Downloaded"));
        downloaded_label.set_widget_name("status-downloaded");
        status_box.pack_end(&downloaded_label, false, false, 0);
    }

    if is_watch_later {
        let watch_later_label = Label::new(Some("Watch Later"));
        watch_later_label.set_widget_name("status-watch-later");
        status_box.pack_end(&watch_later_label, false, false, 0);
    }

    card.pack_start(&status_box, false, false, 0);

    event_box.add(&card);
    event_box
}

fn create_placeholder_image() -> Image {
    let image = Image::from_icon_name(Some("video-x-generic"), gtk::IconSize::Dialog);
    image.set_size_request(320, 180);
    image
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
