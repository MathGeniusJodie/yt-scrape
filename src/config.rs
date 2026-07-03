//! Process configuration read once at startup, so API keys and model choices
//! are not re-read from the environment deep inside call stacks.

const GEMINI_FLASH_MODEL: &str = "gemini-3.5-flash";
const DEFAULT_OPENROUTER_MODELS: [&str; 1] = ["@preset/cheap"];

/// Immutable application configuration sourced from the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `YouTube` Data API key (`GOOGLE_API_KEY`); `None` when unset or blank.
    pub google_api_key: Option<String>,
    /// Gemini API key (`GEMINI_API_KEY`); `None` when unset or blank.
    pub gemini_api_key: Option<String>,
    /// Gemini model name (`GEMINI_MODEL`, with a flash default).
    pub gemini_model: String,
    /// `OpenRouter` API key (`OPENROUTER_API_KEY`); `None` when unset or blank.
    pub openrouter_api_key: Option<String>,
    /// `OpenRouter` model list (`OPENROUTER_MODELS`, comma-separated) with the
    /// first entry primary and the rest fallbacks. Never empty.
    pub openrouter_models: Vec<String>,
}

fn non_empty_env(variable: &str) -> Option<String> {
    std::env::var(variable)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn parse_openrouter_models(raw: Option<String>) -> Vec<String> {
    let models = raw
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if models.is_empty() {
        DEFAULT_OPENROUTER_MODELS.map(str::to_string).into()
    } else {
        models
    }
}

impl Config {
    /// Reads all configuration from the process environment.
    pub fn from_env() -> Self {
        Self {
            google_api_key: non_empty_env("GOOGLE_API_KEY"),
            gemini_api_key: non_empty_env("GEMINI_API_KEY"),
            gemini_model: non_empty_env("GEMINI_MODEL")
                .unwrap_or_else(|| GEMINI_FLASH_MODEL.to_string()),
            openrouter_api_key: non_empty_env("OPENROUTER_API_KEY"),
            openrouter_models: parse_openrouter_models(non_empty_env("OPENROUTER_MODELS")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_openrouter_models;

    #[test]
    fn parse_openrouter_models_splits_and_trims() {
        assert_eq!(
            parse_openrouter_models(Some("a, b ,,c".to_string())),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn parse_openrouter_models_falls_back_to_default() {
        assert_eq!(parse_openrouter_models(None), vec!["@preset/cheap"]);
        assert_eq!(
            parse_openrouter_models(Some(" , ".to_string())),
            vec!["@preset/cheap"]
        );
    }
}
