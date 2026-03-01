use crate::cache::fetch_transcript;
use anyhow::Result;
use async_channel::Sender;
use serde::{Deserialize, Serialize};
use std::path::Path;

const GEMINI_FLASH_MODEL: &str = "gemini-3-flash-preview";
const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const OPENROUTER_MODELS: [&str; 4] = [
    "openrouter/free",
    "moonshotai/kimi-k2",
    "deepseek/deepseek-r1-0528",
    "tngtech/deepseek-r1t2-chimera",
];
const SUMMARY_PROMPT: &str = concat!(
    "Summarize this YouTube video with all the relevant information so I don't have to watch ",
    "it. Don't use nested unordered lists. Don't use underlines. Use headings and bullet ",
    "points where appropriate. Use fancy typography if appropriate, and use italics for ",
    "emphasis or important points. Include memorable quotes in blockquotes. Use * for ",
    "Markdown lists, not -. Include timestamps for sections."
);

#[derive(Serialize)]
struct GeminiRequest {
    contents: Vec<Content>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig,
    tools: Vec<Tool>,
}

#[derive(Serialize)]
struct GenerationConfig {
    #[serde(rename = "thinkingConfig")]
    thinking_config: ThinkingConfig,
}

#[derive(Serialize)]
struct ThinkingConfig {
    #[serde(rename = "thinkingLevel")]
    thinking_level: String,
}

#[derive(Serialize)]
struct Tool {
    #[serde(rename = "urlContext")]
    url_context: UrlContext,
}

#[derive(Serialize)]
struct UrlContext {}

#[derive(Serialize)]
struct Content {
    role: String,
    parts: Vec<Part>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Part {
    Text {
        text: String,
    },
    FileData {
        #[serde(rename = "fileData")]
        file_data: FileData,
    },
}

#[derive(Serialize)]
struct FileData {
    #[serde(rename = "mimeType")]
    mime_type: String,
    #[serde(rename = "fileUri")]
    file_uri: String,
}

#[derive(Deserialize)]
struct GeminiResponse {
    candidates: Option<Vec<Candidate>>,
    error: Option<GeminiError>,
}

#[derive(Deserialize)]
struct Candidate {
    content: CandidateContent,
}

#[derive(Deserialize)]
struct CandidateContent {
    parts: Vec<ResponsePart>,
}

#[derive(Deserialize)]
struct ResponsePart {
    text: Option<String>,
}

#[derive(Deserialize)]
struct GeminiError {
    message: String,
}

#[derive(Serialize)]
struct OpenRouterRequest {
    model: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    models: Vec<String>,
    messages: Vec<OpenRouterMessage>,
}

#[derive(Serialize)]
struct OpenRouterMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct OpenRouterResponse {
    choices: Option<Vec<OpenRouterChoice>>,
    error: Option<OpenRouterError>,
}

#[derive(Deserialize)]
struct OpenRouterChoice {
    message: OpenRouterMessageResponse,
}

#[derive(Deserialize)]
struct OpenRouterMessageResponse {
    content: serde_json::Value,
}

#[derive(Deserialize)]
struct OpenRouterError {
    message: Option<String>,
}

/// Message sent through the streaming channel
#[derive(Debug)]
pub enum StreamingMessage {
    /// Partial text chunk received
    Chunk(String),
    /// Stream completed successfully
    Done,
    /// Error occurred
    Error(String),
}

struct OpenRouterSummaryInput<'a> {
    video_id: &'a str,
    video_url: &'a str,
    video_title: &'a str,
    channel_name: &'a str,
    prompt: &'a str,
    transcripts_work_dir: &'a Path,
}

fn configured_openrouter_models() -> Vec<String> {
    if let Ok(models_raw) = std::env::var("OPENROUTER_MODELS") {
        let models = models_raw
            .split(',')
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if !models.is_empty() {
            return models;
        }
    }

    OPENROUTER_MODELS
        .iter()
        .map(|model| model.to_string())
        .collect()
}

/// Generates an AI summary for a video and streams partial results through a channel.
///
/// The function first attempts Gemini streaming. If Gemini is unavailable or fails,
/// it falls back to OpenRouter using a fetched transcript.
///
/// # Arguments
///
/// * `video_id` - YouTube video identifier.
/// * `video_url` - Full watch URL used by providers.
/// * `video_title` - Human-readable video title for prompt context.
/// * `channel_name` - Channel display name for prompt context.
/// * `transcripts_work_dir` - Temporary directory for transcript extraction artifacts.
/// * `tx` - Channel used to stream [`StreamingMessage`] updates.
pub async fn summarize_video_streaming(
    client: reqwest::Client,
    video_id: &str,
    video_url: &str,
    video_title: &str,
    channel_name: &str,
    transcripts_work_dir: &Path,
    tx: Sender<StreamingMessage>,
) {
    // Try Gemini flash first, then OpenRouter with transcript fallback.
    let gemini_result = match std::env::var("GEMINI_API_KEY") {
        Ok(api_key) => {
            call_gemini_streaming(
                &client,
                &api_key,
                GEMINI_FLASH_MODEL,
                video_url,
                SUMMARY_PROMPT,
                tx.clone(),
            )
            .await
        }
        Err(_) => Err(anyhow::anyhow!(
            "GEMINI_API_KEY environment variable not set"
        )),
    };

    if let Err(gemini_error) = gemini_result {
        if let Err(openrouter_error) = call_openrouter_with_transcript(
            &client,
            OpenRouterSummaryInput {
                video_id,
                video_url,
                video_title,
                channel_name,
                prompt: SUMMARY_PROMPT,
                transcripts_work_dir,
            },
            tx.clone(),
        )
        .await
        {
            let _ = tx
                .send(StreamingMessage::Error(format!(
                    "Gemini failed: {}. OpenRouter fallback failed: {}",
                    gemini_error, openrouter_error
                )))
                .await;
        }
    }
}

async fn call_gemini_streaming(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    video_url: &str,
    prompt: &str,
    tx: Sender<StreamingMessage>,
) -> Result<()> {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
        model
    );

    let request = GeminiRequest {
        contents: vec![Content {
            role: "user".to_string(),
            parts: vec![
                Part::FileData {
                    file_data: FileData {
                        mime_type: "video/*".to_string(),
                        file_uri: video_url.to_string(),
                    },
                },
                Part::Text {
                    text: prompt.to_string(),
                },
            ],
        }],
        generation_config: GenerationConfig {
            thinking_config: ThinkingConfig {
                thinking_level: "LOW".to_string(),
            },
        },
        tools: vec![Tool {
            url_context: UrlContext {},
        }],
    };

    let response = client
        .post(&url)
        .query(&[("key", api_key)])
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        if let Ok(error_response) = serde_json::from_str::<GeminiResponse>(&body) {
            if let Some(error) = error_response.error {
                return Err(anyhow::anyhow!("{}: {}", status, error.message));
            }
        }
        return Err(anyhow::anyhow!("{}: {}", status, body));
    }

    let gemini_response: GeminiResponse = serde_json::from_str(&body)
        .map_err(|e| anyhow::anyhow!("Failed to parse Gemini response: {}", e))?;
    if let Some(error) = &gemini_response.error {
        return Err(anyhow::anyhow!("Gemini API error: {}", error.message));
    }
    let summary = extract_gemini_content(&gemini_response)
        .ok_or_else(|| anyhow::anyhow!("Gemini returned no summary text"))?;

    if tx.send(StreamingMessage::Chunk(summary)).await.is_err() {
        return Ok(());
    }

    let _ = tx.send(StreamingMessage::Done).await;
    Ok(())
}

async fn call_openrouter_with_transcript(
    client: &reqwest::Client,
    input: OpenRouterSummaryInput<'_>,
    tx: Sender<StreamingMessage>,
) -> Result<()> {
    let OpenRouterSummaryInput {
        video_id,
        video_url,
        video_title,
        channel_name,
        prompt,
        transcripts_work_dir,
    } = input;

    let api_key = std::env::var("OPENROUTER_API_KEY")
        .map_err(|_| anyhow::anyhow!("OPENROUTER_API_KEY environment variable not set"))?;

    let transcript = fetch_transcript(video_id, transcripts_work_dir)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to fetch transcript: {}", e))?;

    if transcript.trim().is_empty() {
        anyhow::bail!("Transcript is empty");
    }

    let openrouter_models = configured_openrouter_models();
    if openrouter_models.is_empty() {
        anyhow::bail!("No OpenRouter models configured");
    }

    let request = OpenRouterRequest {
        model: openrouter_models[0].clone(),
        models: openrouter_models.iter().skip(1).cloned().collect(),
        messages: vec![OpenRouterMessage {
            role: "user".to_string(),
            content: format!(
                "{prompt}\n\nVideo title: {video_title}\nChannel: {channel_name}\nVideo URL: {video_url}\n\nTranscript:\n{transcript}"
            ),
        }],
    };

    let response = client
        .post(OPENROUTER_URL)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&request)
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        if let Ok(error_response) = serde_json::from_str::<OpenRouterResponse>(&body) {
            if let Some(error) = error_response.error {
                if let Some(message) = error.message {
                    return Err(anyhow::anyhow!("{}: {}", status, message));
                }
            }
        }
        return Err(anyhow::anyhow!("{}: {}", status, body));
    }

    let openrouter_response: OpenRouterResponse = serde_json::from_str(&body)
        .map_err(|e| anyhow::anyhow!("Failed to parse OpenRouter response: {}", e))?;
    let content = openrouter_response
        .choices
        .and_then(|choices| choices.into_iter().next())
        .and_then(|choice| extract_openrouter_content(&choice.message.content))
        .ok_or_else(|| anyhow::anyhow!("OpenRouter returned no summary text"))?;

    let summary = content.trim().to_string();
    if !summary.is_empty() && tx.send(StreamingMessage::Chunk(summary)).await.is_err() {
        return Ok(());
    }
    let _ = tx.send(StreamingMessage::Done).await;
    Ok(())
}

fn extract_openrouter_content(content: &serde_json::Value) -> Option<String> {
    match content {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(parts) => {
            let merged = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>()
                .join("");
            if merged.trim().is_empty() {
                None
            } else {
                Some(merged)
            }
        }
        _ => None,
    }
}

fn extract_gemini_content(response: &GeminiResponse) -> Option<String> {
    let text = response
        .candidates
        .as_ref()?
        .first()?
        .content
        .parts
        .iter()
        .filter_map(|part| part.text.as_deref())
        .collect::<String>();

    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::{extract_gemini_content, extract_openrouter_content, GeminiResponse};

    #[test]
    fn extract_openrouter_content_reads_string_payload() {
        let payload = serde_json::json!("summary text");
        assert_eq!(
            extract_openrouter_content(&payload),
            Some("summary text".to_string())
        );
    }

    #[test]
    fn extract_openrouter_content_reads_text_parts_array() {
        let payload = serde_json::json!([
            {"type": "text", "text": "Hello "},
            {"type": "text", "text": "world"}
        ]);
        assert_eq!(
            extract_openrouter_content(&payload),
            Some("Hello world".to_string())
        );
    }

    #[test]
    fn extract_openrouter_content_rejects_empty_text_parts() {
        let payload = serde_json::json!([{"type": "text", "text": "   "}]);
        assert_eq!(extract_openrouter_content(&payload), None);
    }

    #[test]
    fn extract_gemini_content_reads_text_parts() {
        let payload = serde_json::json!({
            "candidates": [
                {
                    "content": {
                        "parts": [{"text": "A "}, {"text": "B"}]
                    }
                }
            ]
        });
        let response: GeminiResponse = serde_json::from_value(payload).expect("valid payload");
        assert_eq!(extract_gemini_content(&response), Some("A B".to_string()));
    }

    #[test]
    fn extract_gemini_content_rejects_empty_text() {
        let payload = serde_json::json!({
            "candidates": [
                {
                    "content": {
                        "parts": [{"text": "   "}]
                    }
                }
            ]
        });
        let response: GeminiResponse = serde_json::from_value(payload).expect("valid payload");
        assert_eq!(extract_gemini_content(&response), None);
    }
}
