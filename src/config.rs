use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::ports::KikuError;

/// Top-level configuration loaded from TOML and environment variables.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct KikuConfig {
    pub groq_api_key: Option<String>,
    pub stt_model: String,
    pub post_process_model: String,
    pub activation_key: String,
    pub audio_pipeline: AudioPipelineConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AudioPipelineConfig {
    pub resample_16khz_mono: bool,
    pub noise_suppression: bool,
    pub vad_trim: bool,
    pub compress_opus_or_flac: bool,
}

impl Default for AudioPipelineConfig {
    fn default() -> Self {
        Self {
            resample_16khz_mono: true,
            noise_suppression: true,
            vad_trim: true,
            compress_opus_or_flac: true,
        }
    }
}

impl Default for KikuConfig {
    fn default() -> Self {
        Self {
            groq_api_key: None,
            stt_model: "whisper-large-v3-turbo".to_owned(),
            post_process_model: "llama-3.1-8b-instant".to_owned(),
            activation_key: default_activation_key().to_owned(),
            audio_pipeline: AudioPipelineConfig::default(),
        }
    }
}

fn default_activation_key() -> &'static str {
    if cfg!(target_os = "windows") {
        "RightAlt"
    } else {
        "RightOption"
    }
}

impl KikuConfig {
    /// Returns the standard OS config file path (`~/.config/kiku/config.toml` or `%APPDATA%\kiku\config.toml`).
    pub fn default_path() -> Result<PathBuf, KikuError> {
        #[cfg(target_os = "windows")]
        {
            let base = std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .ok_or_else(|| KikuError::Config("APPDATA is not set".to_owned()))?;
            Ok(base.join("kiku").join("config.toml"))
        }
        #[cfg(not(target_os = "windows"))]
        {
            if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
                return Ok(PathBuf::from(xdg).join("kiku").join("config.toml"));
            }
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| KikuError::Config("HOME is not set".to_owned()))?;
            Ok(home.join(".config").join("kiku").join("config.toml"))
        }
    }

    /// Loads config from `path` (falling back to defaults if the file does not exist).
    pub fn load_from_path(path: &Path) -> Result<Self, KikuError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|e| KikuError::Config(format!("failed to read {}: {e}", path.display())))?;
        toml::from_str(&raw)
            .map_err(|e| KikuError::Config(format!("invalid TOML in {}: {e}", path.display())))
    }

    /// Resolves `GROQ_API_KEY` from the environment first, then from the TOML config.
    pub fn resolve_groq_api_key(&self) -> Result<String, KikuError> {
        self.resolve_groq_api_key_with(|k| std::env::var(k).ok())
    }

    /// Testable variant that accepts an env lookup closure.
    pub fn resolve_groq_api_key_with<F>(&self, get_env: F) -> Result<String, KikuError>
    where
        F: Fn(&str) -> Option<String>,
    {
        if let Some(val) = get_env("GROQ_API_KEY").filter(|s| !s.trim().is_empty()) {
            return Ok(val);
        }
        if let Some(val) = self
            .groq_api_key
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            return Ok(val.to_owned());
        }
        Err(KikuError::Config(
            "GROQ_API_KEY not found in environment or config.toml".to_owned(),
        ))
    }

    /// Writes a default TOML config file if one does not exist yet.
    pub fn write_default_if_missing(path: &Path) -> Result<(), KikuError> {
        if path.exists() {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                KikuError::Config(format!("failed to create {}: {e}", parent.display()))
            })?;
        }
        let content = toml::to_string_pretty(&Self::default())
            .map_err(|e| KikuError::Config(format!("failed to serialize default config: {e}")))?;
        std::fs::write(path, content)
            .map_err(|e| KikuError::Config(format!("failed to write {}: {e}", path.display())))
    }
}
