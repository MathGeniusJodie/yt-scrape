use crate::data::Video;
use crate::urls;
use chrono::{DateTime, Utc};
use quick_xml::events::Event;
use quick_xml::Reader;

/// Parse a YouTube Atom feed XML into Video structs
pub fn parse_feed(xml: &str, channel_id: &str) -> Vec<Video> {
    let mut reader = Reader::from_str(xml);
    let mut videos = Vec::new();

    let mut in_entry = false;
    let mut current_video_id: Option<String> = None;
    let mut current_title: Option<String> = None;
    let mut current_channel_name: Option<String> = None;
    let mut current_published: Option<DateTime<Utc>> = None;
    let mut current_thumbnail: Option<String> = None;
    let mut current_tag: Option<String> = None;
    let mut got_channel_name = false;

    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let local_name_bytes = e.name().local_name().as_ref().to_vec();
                let local_name = std::str::from_utf8(&local_name_bytes).unwrap_or("");

                match local_name {
                    "entry" => {
                        in_entry = true;
                        current_video_id = None;
                        current_title = None;
                        current_published = None;
                        current_thumbnail = None;
                    }
                    "title" => {
                        current_tag = Some("title".to_string());
                    }
                    "name" if in_entry => {
                        current_tag = Some("name".to_string());
                    }
                    "published" if in_entry => {
                        current_tag = Some("published".to_string());
                    }
                    "videoId" => {
                        current_tag = Some("videoId".to_string());
                    }
                    "thumbnail" => {
                        // Extract url attribute from media:thumbnail
                        for attr in e.attributes().flatten() {
                            if attr.key.local_name().as_ref() == b"url" {
                                if let Ok(url) = std::str::from_utf8(&attr.value) {
                                    current_thumbnail = Some(url.to_string());
                                }
                            }
                        }
                    }
                    _ => {}
                }

                // Get channel name from first <name> outside entry (it's the feed author)
                if local_name == "name" && !in_entry && !got_channel_name {
                    current_tag = Some("channel_name".to_string());
                }
            }
            Ok(Event::Text(e)) => {
                if let Some(tag) = &current_tag {
                    if let Ok(text) = e.unescape() {
                        let text = text.trim().to_string();
                        match tag.as_str() {
                            "title" if in_entry => {
                                current_title = Some(text);
                            }
                            "name" if in_entry => {
                                // This is author name inside entry, but we prefer feed-level name
                                if current_channel_name.is_none() {
                                    current_channel_name = Some(text);
                                }
                            }
                            "channel_name" => {
                                current_channel_name = Some(text);
                                got_channel_name = true;
                            }
                            "published" => {
                                if let Ok(dt) = DateTime::parse_from_rfc3339(&text) {
                                    current_published = Some(dt.with_timezone(&Utc));
                                }
                            }
                            "videoId" => {
                                current_video_id = Some(text);
                            }
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                let local_name_bytes = e.name().local_name().as_ref().to_vec();
                let local_name = std::str::from_utf8(&local_name_bytes).unwrap_or("");

                if local_name == "entry" {
                    // Build video if we have all required fields
                    if let (Some(video_id), Some(title), Some(published)) =
                        (&current_video_id, &current_title, &current_published)
                    {
                        let thumbnail_url = current_thumbnail
                            .clone()
                            .unwrap_or_else(|| urls::thumbnail_url(video_id));

                        videos.push(Video {
                            video_id: video_id.clone(),
                            channel_id: channel_id.to_string(),
                            channel_name: current_channel_name
                                .clone()
                                .unwrap_or_else(|| "Unknown".to_string()),
                            title: title.clone(),
                            published: *published,
                            thumbnail_url,
                            duration_seconds: None,
                            transcript: None,
                            ai_summary: None,
                        });
                    }
                    in_entry = false;
                }

                current_tag = None;
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }

        buf.clear();
    }

    videos
}
