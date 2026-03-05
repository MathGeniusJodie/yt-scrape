use crate::cache::fetch_transcript;
use async_channel::Sender;
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

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

#[derive(Debug, Error)]
enum ProviderCallError {
    #[error("missing environment variable {variable}")]
    MissingEnvVar { variable: &'static str },
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("failed to parse {provider} response: {source}")]
    ParseResponse {
        provider: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("{provider} API error: {message}")]
    Api {
        provider: &'static str,
        message: String,
    },
    #[error("{provider} returned no summary text")]
    MissingSummary { provider: &'static str },
    #[error("HTTP {status}: {message}")]
    HttpStatus {
        status: reqwest::StatusCode,
        message: String,
    },
    #[error("failed to fetch transcript: {0}")]
    Transcript(String),
    #[error("transcript is empty")]
    EmptyTranscript,
    #[error("no OpenRouter models configured")]
    NoOpenRouterModels,
}

fn visit_openrouter_content_parts(content: &serde_json::Value, emit: &mut dyn FnMut(&str)) {
    match content {
        serde_json::Value::String(text) => emit(text),
        serde_json::Value::Array(parts) => {
            for part in parts {
                if let Some(text) = part.get("text").and_then(serde_json::Value::as_str) {
                    emit(text);
                }
            }
        }
        _ => {}
    }
}

fn extract_gemini_text(body: &str) -> Result<String, ProviderCallError> {
    let response: GeminiResponse =
        serde_json::from_str(body).map_err(|source| ProviderCallError::ParseResponse {
            provider: "Gemini",
            source,
        })?;

    if let Some(message) = response.error.as_ref().map(|e| e.message.as_str()) {
        return Err(ProviderCallError::Api {
            provider: "Gemini",
            message: message.to_string(),
        });
    }

    let mut merged = String::new();
    if let Some(candidate) = response.candidates.as_ref().and_then(|c| c.first()) {
        for part in &candidate.content.parts {
            if let Some(text) = part.text.as_deref() {
                merged.push_str(text);
            }
        }
    }

    (!merged.trim().is_empty())
        .then_some(merged)
        .ok_or(ProviderCallError::MissingSummary { provider: "Gemini" })
}

fn extract_openrouter_text(body: &str) -> Result<String, ProviderCallError> {
    let response: OpenRouterResponse =
        serde_json::from_str(body).map_err(|source| ProviderCallError::ParseResponse {
            provider: "OpenRouter",
            source,
        })?;

    if let Some(message) = response.error.as_ref().and_then(|e| e.message.as_deref()) {
        return Err(ProviderCallError::Api {
            provider: "OpenRouter",
            message: message.to_string(),
        });
    }

    let mut merged = String::new();
    if let Some(choice) = response.choices.as_ref().and_then(|c| c.first()) {
        visit_openrouter_content_parts(&choice.message.content, &mut |text| merged.push_str(text));
    }

    (!merged.trim().is_empty())
        .then_some(merged)
        .ok_or(ProviderCallError::MissingSummary {
            provider: "OpenRouter",
        })
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

    OPENROUTER_MODELS.map(str::to_string).into()
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
        Err(_) => Err(ProviderCallError::MissingEnvVar {
            variable: "GEMINI_API_KEY",
        }),
    };

    if let Err(gemini_error) = gemini_result {
        if let Err(openrouter_error) = call_openrouter_with_transcript(
            &client,
            video_id,
            video_url,
            video_title,
            channel_name,
            SUMMARY_PROMPT,
            transcripts_work_dir,
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
) -> Result<(), ProviderCallError> {
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
        let message = serde_json::from_str::<GeminiResponse>(&body)
            .ok()
            .and_then(|r| r.error.map(|e| e.message))
            .unwrap_or(body);
        return Err(ProviderCallError::HttpStatus { status, message });
    }
    let summary = extract_gemini_text(&body)?.trim().to_string();

    if tx.send(StreamingMessage::Chunk(summary)).await.is_err() {
        return Ok(());
    }

    let _ = tx.send(StreamingMessage::Done).await;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn call_openrouter_with_transcript(
    client: &reqwest::Client,
    video_id: &str,
    video_url: &str,
    video_title: &str,
    channel_name: &str,
    prompt: &str,
    transcripts_work_dir: &Path,
    tx: Sender<StreamingMessage>,
) -> Result<(), ProviderCallError> {
    let api_key =
        std::env::var("OPENROUTER_API_KEY").map_err(|_| ProviderCallError::MissingEnvVar {
            variable: "OPENROUTER_API_KEY",
        })?;

    let transcript = fetch_transcript(video_id, transcripts_work_dir)
        .await
        .map_err(|error| ProviderCallError::Transcript(error.to_string()))?;

    if transcript.trim().is_empty() {
        return Err(ProviderCallError::EmptyTranscript);
    }

    let openrouter_models = configured_openrouter_models();
    if openrouter_models.is_empty() {
        return Err(ProviderCallError::NoOpenRouterModels);
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
        let message = serde_json::from_str::<OpenRouterResponse>(&body)
            .ok()
            .and_then(|r| r.error.and_then(|e| e.message))
            .unwrap_or(body);
        return Err(ProviderCallError::HttpStatus { status, message });
    }
    let content = extract_openrouter_text(&body)?;

    let summary = content.trim().to_string();
    if !summary.is_empty() && tx.send(StreamingMessage::Chunk(summary)).await.is_err() {
        return Ok(());
    }
    let _ = tx.send(StreamingMessage::Done).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn extract_openrouter_content_reads_string_payload() {
        let payload = serde_json::json!({
            "choices": [{"message": {"content": "summary text"}}]
        });
        let result = super::extract_openrouter_text(&serde_json::to_string(&payload).unwrap());
        assert_eq!(result.unwrap(), "summary text");
    }

    #[test]
    fn extract_openrouter_content_reads_text_parts_array() {
        let payload = serde_json::json!({
            "choices": [{"message": {"content": [
                {"type": "text", "text": "Hello "},
                {"type": "text", "text": "world"}
            ]}}]
        });
        let result = super::extract_openrouter_text(&serde_json::to_string(&payload).unwrap());
        assert_eq!(result.unwrap(), "Hello world");
    }

    #[test]
    fn extract_openrouter_content_rejects_empty_text_parts() {
        let payload = serde_json::json!({
            "choices": [{"message": {"content": [{"type": "text", "text": "   "}]}}]
        });
        let result = super::extract_openrouter_text(&serde_json::to_string(&payload).unwrap());
        assert!(result.is_err());
    }

    #[test]
    fn extract_gemini_content_reads_text_parts() {
        let payload = serde_json::json!({
            "candidates": [{"content": {"parts": [{"text": "A "}, {"text": "B"}]}}]
        });
        let result = super::extract_gemini_text(&serde_json::to_string(&payload).unwrap());
        assert_eq!(result.unwrap(), "A B");
    }

    #[test]
    fn extract_gemini_content_rejects_empty_text() {
        let payload = serde_json::json!({
            "candidates": [{"content": {"parts": [{"text": "   "}]}}]
        });
        let result = super::extract_gemini_text(&serde_json::to_string(&payload).unwrap());
        assert!(result.is_err());
    }
}
