use anyhow::Result;
use serde::{Deserialize, Serialize};

const GEMINI_PRO_MODEL: &str = "gemini-3-pro-preview";
const GEMINI_FLASH_MODEL: &str = "gemini-3-flash-preview";

#[derive(Debug, Clone)]
pub enum SummaryState {
    Loading,
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
    text: String,
}

#[derive(Deserialize)]
struct GeminiError {
    message: String,
    #[allow(dead_code)]
    status: Option<String>,
}

pub async fn summarize_video(video_url: &str, _video_title: &str, _channel_name: &str) -> Result<String> {
    let api_key = std::env::var("GEMINI_API_KEY")
        .map_err(|_| anyhow::anyhow!("GEMINI_API_KEY environment variable not set"))?;

    let prompt = format!(
        "Summarize this YouTube video with all the relevant information so I don't have to watch it",
        //video_title, channel_name
    );

    // Try pro model first, fall back to flash on rate limit
    match call_gemini(&api_key, GEMINI_PRO_MODEL, video_url, &prompt).await {
        Ok(summary) => Ok(summary),
        Err(e) => {
            let error_str = e.to_string();
            // Check for rate limit (429) or quota exceeded
            if error_str.contains("429") || error_str.contains("RESOURCE_EXHAUSTED") || error_str.contains("quota") {
                // Fall back to flash model
                call_gemini(&api_key, GEMINI_FLASH_MODEL, video_url, &prompt).await
            } else {
                Err(e)
            }
        }
    }
}

async fn call_gemini(api_key: &str, model: &str, video_url: &str, prompt: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
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
    let body = response.text().await?;

    if !status.is_success() {
        // Try to parse error message
        if let Ok(error_response) = serde_json::from_str::<GeminiResponse>(&body) {
            if let Some(error) = error_response.error {
                return Err(anyhow::anyhow!("{}: {}", status, error.message));
            }
        }
        return Err(anyhow::anyhow!("{}: {}", status, body));
    }

    let response: GeminiResponse = serde_json::from_str(&body)?;

    if let Some(candidates) = response.candidates {
        if let Some(candidate) = candidates.first() {
            if let Some(part) = candidate.content.parts.first() {
                return Ok(part.text.clone());
            }
        }
    }

    Err(anyhow::anyhow!("No response content from Gemini"))
}
