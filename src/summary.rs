use crate::cache::{fetch_transcript, transcript_from_vtt_file};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

// Summarizing a long video can far exceed the app-wide 120s HTTP timeout, so
// provider requests override it per-request.
const SUMMARY_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
const GEMINI_FLASH_MODEL: &str = "gemini-3.5-flash";
const GEMINI_THINKING_LEVEL: &str = "minimal";
const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const OPENROUTER_MODELS: [&str; 1] = ["@preset/cheap"];
const OPENROUTER_REASONING_EFFORT: &str = "none";
const SUMMARY_PROMPT: &str = concat!(
    "Summarize this YouTube video with all the relevant information so I don't have to watch. ",
    "Make sure to include the nuggets of useful knowledge/wisdom and any funny quotes.",
);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiRequest {
    contents: Vec<Content>,
    generation_config: GenerationConfig,
    tools: Vec<Tool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationConfig {
    thinking_config: ThinkingConfig,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ThinkingConfig {
    thinking_level: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Tool {
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
#[serde(rename_all = "camelCase")]
struct FileData {
    mime_type: String,
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
    reasoning: OpenRouterReasoning,
    messages: Vec<OpenRouterMessage>,
}

#[derive(Serialize)]
struct OpenRouterReasoning {
    effort: &'static str,
    exclude: bool,
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
    Http(reqwest::Error),
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
    #[error("transcript is empty")]
    EmptyTranscript,
    #[error("no OpenRouter models configured")]
    NoOpenRouterModels,
}

impl From<reqwest::Error> for ProviderCallError {
    // Strip the URL so API keys in query strings never reach logs or the UI.
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error.without_url())
    }
}

/// Result of a successful summarization attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummaryOutcome {
    /// An AI-generated summary. Safe to cache as the video's summary.
    Summary(String),
    /// All AI providers failed but a transcript was fetched. Must NOT be cached
    /// as a summary; callers may cache it as the transcript and display it.
    TranscriptOnly(String),
}

/// Errors from a failed summarization attempt (all providers and fallbacks exhausted).
#[derive(Debug, Error)]
pub enum SummaryError {
    /// Gemini failed and the transcript fallback also failed.
    #[error("Gemini failed: {gemini}. Transcript fallback failed: {transcript}")]
    NoSummaryOrTranscript {
        /// Gemini failure description.
        gemini: String,
        /// Transcript failure description.
        transcript: String,
    },
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

fn gemini_model() -> String {
    std::env::var("GEMINI_MODEL").unwrap_or_else(|_| GEMINI_FLASH_MODEL.to_string())
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

/// Inputs for [`summarize_video`].
#[derive(Debug, Clone)]
pub struct SummarizeRequest {
    /// `YouTube` video identifier.
    pub video_id: String,
    /// Full watch URL used by providers.
    pub video_url: String,
    /// Human-readable video title for prompt context.
    pub video_title: String,
    /// Channel display name for prompt context.
    pub channel_name: String,
    /// Temporary directory for transcript extraction artifacts.
    pub transcripts_work_dir: PathBuf,
    /// Already-downloaded `.vtt` subtitle file, reused before shelling out
    /// another yt-dlp subtitle request.
    pub local_subtitle_path: Option<PathBuf>,
}

/// Resolves a transcript, preferring a local subtitle file over a fresh yt-dlp fetch.
async fn resolve_transcript(request: &SummarizeRequest) -> Result<String, String> {
    if let Some(subtitle_path) = request.local_subtitle_path.as_deref()
        && let Some(transcript) = transcript_from_vtt_file(subtitle_path).await
    {
        return Ok(transcript);
    }

    match fetch_transcript(&request.video_id, &request.transcripts_work_dir).await {
        Ok(transcript) if !transcript.trim().is_empty() => Ok(transcript),
        Ok(_) => Err(ProviderCallError::EmptyTranscript.to_string()),
        Err(transcript_error) => Err(transcript_error.to_string()),
    }
}

/// Generates an AI summary for a video.
///
/// Tries Gemini first. If Gemini is unavailable or fails, resolves the transcript
/// (preferring a local subtitle file) and tries `OpenRouter`. If `OpenRouter` also
/// fails, returns the transcript itself as [`SummaryOutcome::TranscriptOnly`] so
/// callers can still show something useful without mistaking it for an AI summary.
///
/// # Arguments
///
/// * `client` - HTTP client used for provider requests.
/// * `request` - Video metadata and transcript sources (see [`SummarizeRequest`]).
///
/// # Errors
///
/// Returns [`SummaryError`] when every provider and the transcript fallback fail.
pub async fn summarize_video(
    client: &reqwest::Client,
    request: &SummarizeRequest,
) -> Result<SummaryOutcome, SummaryError> {
    let gemini_result = match std::env::var("GEMINI_API_KEY") {
        Ok(api_key) => {
            call_gemini(
                client,
                &api_key,
                &gemini_model(),
                &request.video_url,
                SUMMARY_PROMPT,
            )
            .await
        }
        Err(_) => Err(ProviderCallError::MissingEnvVar {
            variable: "GEMINI_API_KEY",
        }),
    };

    let gemini_error = match gemini_result {
        Ok(summary) => return Ok(SummaryOutcome::Summary(summary)),
        Err(gemini_error) => gemini_error,
    };

    let transcript = match resolve_transcript(request).await {
        Ok(transcript) => transcript,
        Err(transcript_error) => {
            return Err(SummaryError::NoSummaryOrTranscript {
                gemini: gemini_error.to_string(),
                transcript: transcript_error,
            });
        }
    };

    match call_openrouter_with_transcript(client, request, SUMMARY_PROMPT, &transcript).await {
        Ok(summary) => Ok(SummaryOutcome::Summary(summary)),
        Err(openrouter_error) => {
            log::warn!(
                "OpenRouter summary failed for {} (falling back to transcript): \
                 {openrouter_error}",
                request.video_id
            );
            Ok(SummaryOutcome::TranscriptOnly(
                transcript.trim().to_string(),
            ))
        }
    }
}

async fn call_gemini(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    video_url: &str,
    prompt: &str,
) -> Result<String, ProviderCallError> {
    let url =
        format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent");

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
                thinking_level: GEMINI_THINKING_LEVEL.to_string(),
            },
        },
        tools: vec![Tool {
            url_context: UrlContext {},
        }],
    };

    let response = client
        .post(&url)
        .timeout(SUMMARY_REQUEST_TIMEOUT)
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
    Ok(extract_gemini_text(&body)?.trim().to_string())
}

async fn call_openrouter_with_transcript(
    client: &reqwest::Client,
    request: &SummarizeRequest,
    prompt: &str,
    transcript: &str,
) -> Result<String, ProviderCallError> {
    let api_key =
        std::env::var("OPENROUTER_API_KEY").map_err(|_| ProviderCallError::MissingEnvVar {
            variable: "OPENROUTER_API_KEY",
        })?;

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
        reasoning: OpenRouterReasoning {
            effort: OPENROUTER_REASONING_EFFORT,
            exclude: true,
        },
        messages: vec![OpenRouterMessage {
            role: "user".to_string(),
            content: format!(
                "{prompt}\n\nVideo title: {}\nChannel: {}\nVideo URL: {}\n\nTranscript:\n{transcript}",
                request.video_title, request.channel_name, request.video_url
            ),
        }],
    };

    let response = client
        .post(OPENROUTER_URL)
        .timeout(SUMMARY_REQUEST_TIMEOUT)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {api_key}"))
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
    Ok(extract_openrouter_text(&body)?.trim().to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn gemini_request_sets_minimal_thinking_level() {
        let request = super::GeminiRequest {
            contents: Vec::new(),
            generation_config: super::GenerationConfig {
                thinking_config: super::ThinkingConfig {
                    thinking_level: super::GEMINI_THINKING_LEVEL.to_string(),
                },
            },
            tools: Vec::new(),
        };

        let payload = serde_json::to_value(request).expect("request should serialize");

        assert_eq!(
            payload["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            serde_json::json!("minimal")
        );
    }

    #[test]
    fn openrouter_request_disables_reasoning() {
        let request = super::OpenRouterRequest {
            model: "@preset/cheap".to_string(),
            models: Vec::new(),
            reasoning: super::OpenRouterReasoning {
                effort: super::OPENROUTER_REASONING_EFFORT,
                exclude: true,
            },
            messages: Vec::new(),
        };

        let payload = serde_json::to_value(request).expect("request should serialize");

        assert_eq!(payload["reasoning"]["effort"], serde_json::json!("none"));
        assert_eq!(payload["reasoning"]["exclude"], serde_json::json!(true));
    }

    #[test]
    fn extract_openrouter_content_reads_string_payload() {
        let payload = serde_json::json!({
            "choices": [{"message": {"content": "summary text"}}]
        });
        let result = super::extract_openrouter_text(&payload.to_string());
        assert_eq!(result.expect("payload should parse"), "summary text");
    }

    #[test]
    fn extract_openrouter_content_reads_text_parts_array() {
        let payload = serde_json::json!({
            "choices": [{"message": {"content": [
                {"type": "text", "text": "Hello "},
                {"type": "text", "text": "world"}
            ]}}]
        });
        let result = super::extract_openrouter_text(&payload.to_string());
        assert_eq!(result.expect("payload should parse"), "Hello world");
    }

    #[test]
    fn extract_openrouter_content_rejects_empty_text_parts() {
        let payload = serde_json::json!({
            "choices": [{"message": {"content": [{"type": "text", "text": "   "}]}}]
        });
        let result = super::extract_openrouter_text(&payload.to_string());
        assert!(result.is_err());
    }

    #[test]
    fn extract_gemini_content_reads_text_parts() {
        let payload = serde_json::json!({
            "candidates": [{"content": {"parts": [{"text": "A "}, {"text": "B"}]}}]
        });
        let result = super::extract_gemini_text(&payload.to_string());
        assert_eq!(result.expect("payload should parse"), "A B");
    }

    #[test]
    fn extract_gemini_content_rejects_empty_text() {
        let payload = serde_json::json!({
            "candidates": [{"content": {"parts": [{"text": "   "}]}}]
        });
        let result = super::extract_gemini_text(&payload.to_string());
        assert!(result.is_err());
    }
}
