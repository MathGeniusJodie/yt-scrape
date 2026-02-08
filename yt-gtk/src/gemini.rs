use anyhow::Result;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

const GEMINI_PRO_MODEL: &str = "gemini-3-pro-preview";
const GEMINI_FLASH_MODEL: &str = "gemini-3-flash-preview";

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

pub async fn summarize_video_streaming(
    video_url: &str,
    _video_title: &str,
    _channel_name: &str,
    tx: mpsc::UnboundedSender<StreamingMessage>,
) {
    let api_key = match std::env::var("GEMINI_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            let _ = tx.send(StreamingMessage::Error(
                "GEMINI_API_KEY environment variable not set".to_string(),
            ));
            return;
        }
    };

    let prompt = "Summarize this YouTube video with all the relevant information so I don't have to watch it. Don't use nested unordered lists. don't use underlines. use heading and bullet points where appropriate. use fancy typography if appropriate, use italic for emphasis/important points. Include memorable quotes in blockquotes. Use * for markdown list, not -.include timestamps for sections".to_string();

    // Try pro model first, fall back to flash on rate limit
    match call_gemini_streaming(&api_key, GEMINI_PRO_MODEL, video_url, &prompt, tx.clone()).await {
        Ok(()) => {}
        Err(e) => {
            let error_str = e.to_string();
            // Check for rate limit (429) or quota exceeded
            if error_str.contains("429")
                || error_str.contains("RESOURCE_EXHAUSTED")
                || error_str.contains("quota")
            {
                // Fall back to flash model
                if let Err(e) = call_gemini_streaming(
                    &api_key,
                    GEMINI_FLASH_MODEL,
                    video_url,
                    &prompt,
                    tx.clone(),
                )
                .await
                {
                    let _ = tx.send(StreamingMessage::Error(e.to_string()));
                }
            } else {
                let _ = tx.send(StreamingMessage::Error(error_str));
            }
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
