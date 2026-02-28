use crate::cache::fetch_transcript;
use anyhow::Result;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::sync::mpsc;

const GEMINI_FLASH_MODEL: &str = "gemini-3-flash-preview";
const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const OPENROUTER_MODELS: [&str; 4] = [
    "openrouter/free",
    "moonshotai/kimi-k2",
    "deepseek/deepseek-r1-0528",
    "tngtech/deepseek-r1t2-chimera",
];
const SUMMARY_PROMPT: &str = "Summarize this YouTube video with all the relevant information so I don't have to watch it. Don't use nested unordered lists. don't use underlines. use heading and bullet points where appropriate. use fancy typography if appropriate, use italic for emphasis/important points. Include memorable quotes in blockquotes. Use * for markdown list, not -.include timestamps for sections";

/// High-level status of a summary request for UI state handling.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum SummaryState {
    Loading,
    Streaming(String),
    Ready(String),
    Error(String),
}

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
    #[allow(dead_code)]
    status: Option<String>,
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
    video_id: &str,
    video_url: &str,
    video_title: &str,
    channel_name: &str,
    transcripts_work_dir: &Path,
    tx: mpsc::UnboundedSender<StreamingMessage>,
) {
    let prompt = SUMMARY_PROMPT.to_string();

    // Try Gemini flash first, then OpenRouter with transcript fallback.
    let gemini_result = match std::env::var("GEMINI_API_KEY") {
        Ok(api_key) => {
            call_gemini_streaming(&api_key, GEMINI_FLASH_MODEL, video_url, &prompt, tx.clone())
                .await
        }
        Err(_) => Err(anyhow::anyhow!(
            "GEMINI_API_KEY environment variable not set"
        )),
    };

    if let Err(gemini_error) = gemini_result {
        if let Err(openrouter_error) = call_openrouter_with_transcript(
            video_id,
            video_url,
            video_title,
            channel_name,
            &prompt,
            transcripts_work_dir,
            tx.clone(),
        )
        .await
        {
            let _ = tx.send(StreamingMessage::Error(format!(
                "Gemini failed: {}. OpenRouter fallback failed: {}",
                gemini_error, openrouter_error
            )));
        }
    }
}

async fn call_gemini_streaming(
    api_key: &str,
    model: &str,
    video_url: &str,
    prompt: &str,
    tx: mpsc::UnboundedSender<StreamingMessage>,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:streamGenerateContent?key={}&alt=sse",
        model, api_key
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
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await?;
        if let Ok(error_response) = serde_json::from_str::<GeminiResponse>(&body) {
            if let Some(error) = error_response.error {
                return Err(anyhow::anyhow!("{}: {}", status, error.message));
            }
        }
        return Err(anyhow::anyhow!("{}: {}", status, body));
    }

    // Process the SSE stream
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();

    while let Some(result) = stream.next().await {
        let chunk = result?;
        let text = String::from_utf8_lossy(&chunk);
        buffer.push_str(&text);

        // Process complete SSE events - normalize line endings first
        let normalized = buffer.replace("\r\n", "\n");
        buffer = normalized;

        // Process complete SSE events (data: {...}\n\n)
        while let Some(event_end) = buffer.find("\n\n") {
            let event = buffer[..event_end].to_string();
            buffer = buffer[event_end + 2..].to_string();

            // Parse the SSE event - handle potential leading newlines
            let event = event.trim_start_matches('\n');
            if let Some(data) = event.strip_prefix("data: ") {
                match serde_json::from_str::<GeminiResponse>(data) {
                    Ok(response) => {
                        if let Some(candidates) = response.candidates {
                            if let Some(candidate) = candidates.first() {
                                for part in &candidate.content.parts {
                                    if let Some(ref text) = part.text {
                                        let text = smartify_quotes(text);
                                        if !text.is_empty() {
                                            let _ = tx.send(StreamingMessage::Chunk(text));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(_) => {
                        // Ignore parse errors for incomplete chunks
                    }
                }
            }
        }
    }

    let _ = tx.send(StreamingMessage::Done);
    Ok(())
}

async fn call_openrouter_with_transcript(
    video_id: &str,
    video_url: &str,
    video_title: &str,
    channel_name: &str,
    prompt: &str,
    transcripts_work_dir: &Path,
    tx: mpsc::UnboundedSender<StreamingMessage>,
) -> Result<()> {
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

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;

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

    let summary = smartify_quotes(content.trim());
    if !summary.is_empty() {
        let _ = tx.send(StreamingMessage::Chunk(summary));
    }
    let _ = tx.send(StreamingMessage::Done);
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

/// Convert straight quotes to curly/smart quotes
fn smartify_quotes(text: &str) -> String {
    use smart_quotes::{decide_quote_after, Decision};

    const OPEN_DOUBLE: char = '\u{201C}'; // "
    const CLOSE_DOUBLE: char = '\u{201D}'; // "
    const OPEN_SINGLE: char = '\u{2018}'; // '
    const CLOSE_SINGLE: char = '\u{2019}'; // ' (also apostrophe)

    let mut result = String::with_capacity(text.len());
    let mut prev_char: Option<char> = None;

    for c in text.chars() {
        match c {
            '"' => match decide_quote_after(prev_char) {
                Decision::Open => result.push(OPEN_DOUBLE),
                Decision::Close => result.push(CLOSE_DOUBLE),
            },
            '\'' => match decide_quote_after(prev_char) {
                Decision::Open => result.push(OPEN_SINGLE),
                Decision::Close => result.push(CLOSE_SINGLE),
            },
            _ => result.push(c),
        }
        prev_char = Some(c);
    }

    result
}
