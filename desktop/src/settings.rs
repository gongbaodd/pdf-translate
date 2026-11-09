//! Non-secret settings persisted as JSON in the platform config directory.
//! Credentials are never stored here.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    Codex,
    #[serde(rename = "opencode-go")]
    OpenCodeGo,
    ClaudeCode,
    OpenaiCompatible,
}

impl Backend {
    pub const ALL: [Backend; 4] = [
        Backend::Codex,
        Backend::OpenCodeGo,
        Backend::ClaudeCode,
        Backend::OpenaiCompatible,
    ];

    /// Value for `--backend` / `--provider`.
    pub fn cli_name(self) -> &'static str {
        match self {
            Backend::Codex => "codex",
            Backend::OpenCodeGo => "opencode-go",
            Backend::ClaudeCode => "claude-code",
            Backend::OpenaiCompatible => "openai-compatible",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Backend::Codex => "Codex",
            Backend::OpenCodeGo => "OpenCode Go",
            Backend::ClaudeCode => "Claude Code",
            Backend::OpenaiCompatible => "OpenAI-compatible",
        }
    }

    /// Backends served through the local TypeScript bridge.
    pub fn uses_bridge(self) -> bool {
        !matches!(self, Backend::OpenaiCompatible)
    }

    /// Backends with an in-app sign-in flow (`login --events json`).
    pub fn has_login(self) -> bool {
        matches!(self, Backend::Codex | Backend::OpenCodeGo)
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Backend::Codex | Backend::OpenCodeGo => "gpt-5.6-luna",
            Backend::ClaudeCode => "sonnet",
            Backend::OpenaiCompatible => "gpt-4o-mini",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputMode {
    Bilingual,
    TranslatedOnly,
}

impl OutputMode {
    pub fn cli_name(self) -> &'static str {
        match self {
            OutputMode::Bilingual => "bilingual",
            OutputMode::TranslatedOnly => "translated-only",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            OutputMode::Bilingual => "Side by side",
            OutputMode::TranslatedOnly => "Chinese only",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub backend: Backend,
    pub model: String,
    pub output_mode: OutputMode,
    pub output_dir: String,
    pub glossary: String,
    pub concurrency: u32,
    pub code_font_pattern: String,
    pub base_url: String,
    pub api_key_env: String,
    pub claude_bin: String,
}

impl Default for Settings {
    fn default() -> Self {
        let output_dir = directories::UserDirs::new()
            .and_then(|dirs| dirs.document_dir().map(|dir| dir.join("pi-docs")))
            .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().join("pi-docs")))
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        Self {
            backend: Backend::Codex,
            model: Backend::Codex.default_model().into(),
            output_mode: OutputMode::Bilingual,
            output_dir,
            glossary: String::new(),
            concurrency: 1,
            code_font_pattern: String::new(),
            base_url: "https://api.openai.com/v1".into(),
            api_key_env: "OPENAI_API_KEY".into(),
            claude_bin: String::new(),
        }
    }
}

fn settings_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "pi-docs", "pi-docs")
        .map(|dirs| dirs.config_dir().join("settings.json"))
}

impl Settings {
    pub fn load() -> Self {
        settings_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = settings_path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(text) = serde_json::to_string_pretty(self) else {
            return;
        };
        let temporary = path.with_extension("json.tmp");
        if std::fs::write(&temporary, text).is_ok() {
            let _ = std::fs::rename(&temporary, &path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_settings_fall_back_to_defaults() {
        let settings: Settings = serde_json::from_str(r#"{"backend":"claude-code","model":"opus"}"#).unwrap();
        assert_eq!(settings.backend, Backend::ClaudeCode);
        assert_eq!(settings.model, "opus");
        assert_eq!(settings.concurrency, 1);
    }

    #[test]
    fn backend_names_match_the_python_cli() {
        let names: Vec<_> = Backend::ALL.iter().map(|b| b.cli_name()).collect();
        assert_eq!(names, ["codex", "opencode-go", "claude-code", "openai-compatible"]);
        for backend in Backend::ALL {
            let json = serde_json::to_string(&backend).unwrap();
            assert_eq!(json, format!("\"{}\"", backend.cli_name()));
        }
    }
}
