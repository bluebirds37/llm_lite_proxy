use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::{fmt, io};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Openai,
    Anthropic,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::Openai => "https://api.openai.com/v1",
            Self::Anthropic => "https://api.anthropic.com",
        }
    }

    pub fn default_api(self) -> Api {
        Api::default_for(self)
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Which concrete API surface speaks a given wire format. `Openai` and
/// `Anthropic` are families that decide auth headers and the error envelope;
/// the surface decides the endpoint path and the conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Api {
    Chat,
    Responses,
    Messages,
}

impl Api {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Responses => "responses",
            Self::Messages => "messages",
        }
    }

    pub fn path(self) -> &'static str {
        match self {
            Self::Chat => "chat/completions",
            Self::Responses => "responses",
            Self::Messages => "messages",
        }
    }

    pub fn family(self) -> Protocol {
        match self {
            Self::Chat | Self::Responses => Protocol::Openai,
            Self::Messages => Protocol::Anthropic,
        }
    }

    pub fn default_for(protocol: Protocol) -> Self {
        match protocol {
            Protocol::Openai => Self::Chat,
            Protocol::Anthropic => Self::Messages,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Chat => "OpenAI Chat Completions",
            Self::Responses => "OpenAI Responses",
            Self::Messages => "Anthropic Messages",
        }
    }

    pub fn all() -> [Api; 3] {
        [Self::Chat, Self::Responses, Self::Messages]
    }
}

impl fmt::Display for Api {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelRoute {
    /// The name a client sends.
    pub public: String,
    /// The name sent upstream.
    pub upstream: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Provider {
    pub name: String,
    pub protocol: Protocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<Api>,
    pub base_url: String,
    pub api_key: String,
    pub enabled: bool,
    /// Glob patterns matched against the inbound model name, tried in order.
    pub patterns: Vec<String>,
    pub models: Vec<ModelRoute>,
    /// Extra headers merged into every upstream request, e.g. a gateway's
    /// `anthropic-version` or a `User-Agent` it insists on.
    pub headers: BTreeMap<String, String>,
}

impl Default for Provider {
    fn default() -> Self {
        Self {
            name: String::new(),
            protocol: Protocol::Openai,
            api: None,
            base_url: String::new(),
            api_key: String::new(),
            enabled: true,
            patterns: Vec::new(),
            models: Vec::new(),
            headers: BTreeMap::new(),
        }
    }
}

impl Provider {
    pub fn api(&self) -> Api {
        self.api.unwrap_or_else(|| Api::default_for(self.protocol))
    }

    /// Gateways mount the two APIs at different depths, so a base URL may be
    /// given as a bare host, a `/v1` prefix, or the exact endpoint.
    pub fn url(&self) -> String {
        let trimmed = self.base_url.trim().trim_end_matches('/');
        let base = if trimmed.is_empty() {
            self.protocol.default_base_url()
        } else {
            trimmed
        };
        let endpoint = self.api().path();
        if base.ends_with(endpoint) {
            return base.to_string();
        }
        match self.api() {
            Api::Messages if base.ends_with("/v1") => format!("{base}/messages"),
            Api::Messages => format!("{base}/v1/{endpoint}"),
            _ if base.ends_with("/v1") => format!("{base}/{endpoint}"),
            _ => format!("{base}/v1/{endpoint}"),
        }
    }

    pub fn upstream_model(&self, requested: &str) -> String {
        self.models
            .iter()
            .find(|route| route.public == requested)
            .map(|route| route.upstream.clone())
            .unwrap_or_else(|| requested.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Server {
    pub host: String,
    pub port: u16,
    /// Keys clients must present. Empty means the proxy is open, which is fine
    /// for a loopback-only setup.
    pub master_keys: Vec<String>,
    pub default_provider: Option<String>,
    pub default_max_tokens: u32,
    pub passthrough_unknown_params: bool,
    pub openai_include_usage: bool,
    pub request_timeout_secs: u64,
    /// How long a streamed response may go without any upstream bytes.
    pub stream_idle_timeout_secs: u64,
    pub max_body_mb: u32,
    pub log_capacity: usize,
    /// Origins allowed to call the proxy from a browser. Empty blocks browser
    /// callers, `["*"]` allows any.
    pub allow_origins: Vec<String>,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8787,
            master_keys: Vec::new(),
            default_provider: None,
            default_max_tokens: 4096,
            passthrough_unknown_params: false,
            openai_include_usage: true,
            request_timeout_secs: 600,
            stream_idle_timeout_secs: 120,
            max_body_mb: 64,
            log_capacity: 400,
            allow_origins: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: Server,
    pub providers: Vec<Provider>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{0}")]
    Invalid(String),
    #[error("could not read {}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("could not write {}: {source}", path.display())]
    Write { path: PathBuf, source: io::Error },
    #[error("could not parse {}: {message}", path.display())]
    Parse { path: PathBuf, message: String },
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_str(&text).map_err(|error| ConfigError::Parse {
            path: path.to_path_buf(),
            message: error.to_string(),
        })
    }

    /// Written through a sibling file so a crash mid-write cannot truncate the
    /// only copy of the operator's keys.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let mut text = serde_json::to_string_pretty(self).map_err(|error| ConfigError::Write {
            path: path.to_path_buf(),
            source: io::Error::other(error.to_string()),
        })?;
        text.push('\n');
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, text).map_err(|source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        })?;
        std::fs::rename(&temp, path).map_err(|source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.server.port == 0 {
            return Err(ConfigError::Invalid(
                "server.port must be between 1 and 65535".into(),
            ));
        }
        if self.server.host.trim().is_empty() {
            return Err(ConfigError::Invalid("server.host must not be empty".into()));
        }
        let mut names: Vec<String> = Vec::new();
        for provider in &self.providers {
            let name = provider.name.trim();
            if name.is_empty() {
                return Err(ConfigError::Invalid("every provider needs a name".into()));
            }
            if names.iter().any(|seen| seen.eq_ignore_ascii_case(name)) {
                return Err(ConfigError::Invalid(format!(
                    "provider name `{name}` is used twice"
                )));
            }
            if name.contains('/') || name.contains(' ') {
                return Err(ConfigError::Invalid(format!(
                    "provider name `{name}` cannot contain / or spaces"
                )));
            }
            if let Some(api) = provider.api {
                if api.family() != provider.protocol {
                    return Err(ConfigError::Invalid(format!(
                        "provider `{name}` cannot serve {} over an {} endpoint",
                        provider.protocol, api
                    )));
                }
            }
            names.push(name.to_string());
            let base = if provider.base_url.trim().is_empty() {
                provider.protocol.default_base_url()
            } else {
                provider.base_url.trim()
            };
            if !(base.starts_with("http://") || base.starts_with("https://")) {
                return Err(ConfigError::Invalid(format!(
                    "provider `{name}` base_url must start with http:// or https://"
                )));
            }
            for route in &provider.models {
                if route.upstream.trim().is_empty() || route.public.trim().is_empty() {
                    return Err(ConfigError::Invalid(format!(
                        "provider `{name}` has a model route with an empty public or upstream name"
                    )));
                }
            }
        }
        if let Some(default) = &self.server.default_provider {
            if !names.iter().any(|seen| seen == default) {
                return Err(ConfigError::Invalid(format!(
                    "server.default_provider `{default}` does not match any provider name"
                )));
            }
        }
        Ok(())
    }

    pub fn options(&self) -> protocol::Options {
        protocol::Options {
            default_max_tokens: self.server.default_max_tokens,
            passthrough_extras: self.server.passthrough_unknown_params,
            openai_include_usage: self.server.openai_include_usage,
        }
    }

    /// A first-run setup with one of each protocol so the GUI is never empty.
    pub fn example() -> Self {
        Self {
            server: Server::default(),
            providers: vec![
                Provider {
                    name: "claude".into(),
                    protocol: Protocol::Anthropic,
                    api: Some(Api::Messages),
                    base_url: Protocol::Anthropic.default_base_url().into(),
                    api_key: String::new(),
                    patterns: vec!["claude*".into()],
                    models: vec![ModelRoute {
                        public: "claude-sonnet-4-5".into(),
                        upstream: "claude-sonnet-4-5".into(),
                        provider: None,
                    }],
                    ..Default::default()
                },
                Provider {
                    name: "openai".into(),
                    protocol: Protocol::Openai,
                    api: Some(Api::Chat),
                    base_url: Protocol::Openai.default_base_url().into(),
                    api_key: String::new(),
                    patterns: vec!["gpt*".into()],
                    models: vec![ModelRoute {
                        public: "gpt-4o-mini".into(),
                        upstream: "gpt-4o-mini".into(),
                        provider: None,
                    }],
                    ..Default::default()
                },
            ],
        }
    }
}

/// `*` and `?` only; model names never need more.
pub fn matches_glob(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.to_ascii_lowercase().chars().collect();
    let text: Vec<char> = text.to_ascii_lowercase().chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut star_text) = (usize::MAX, 0usize);

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = p;
            star_text = t;
            p += 1;
        } else if star != usize::MAX {
            p = star + 1;
            star_text += 1;
            t = star_text;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_built_from_any_base_shape() {
        let mut provider = Provider {
            name: "a".into(),
            protocol: Protocol::Anthropic,
            base_url: "https://api.anthropic.com".into(),
            ..Default::default()
        };
        assert_eq!(provider.url(), "https://api.anthropic.com/v1/messages");

        provider.base_url = "https://gateway.example.com/api/anthropic/v1/".into();
        assert_eq!(
            provider.url(),
            "https://gateway.example.com/api/anthropic/v1/messages"
        );

        provider.base_url = "https://gateway.example.com/v1/messages".into();
        assert_eq!(provider.url(), "https://gateway.example.com/v1/messages");

        provider.protocol = Protocol::Openai;
        provider.base_url = "https://api.deepseek.com/v1".into();
        assert_eq!(
            provider.url(),
            "https://api.deepseek.com/v1/chat/completions"
        );
    }

    #[test]
    fn glob_matching_covers_the_patterns_the_gui_offers() {
        assert!(matches_glob("claude*", "claude-sonnet-4-5"));
        assert!(matches_glob("gpt-?o", "gpt-4o"));
        assert!(matches_glob("*router*", "openrouter/claude"));
        assert!(matches_glob("kimi", "kimi"));
        assert!(!matches_glob("claude*", "gpt-4o"));
        assert!(!matches_glob("claude", "claude-3"));
    }

    #[test]
    fn model_routes_rename_only_the_public_name() {
        let provider = Provider {
            name: "fast".into(),
            protocol: Protocol::Openai,
            models: vec![ModelRoute {
                public: "cheap".into(),
                upstream: "gpt-4o-mini".into(),
                provider: None,
            }],
            ..Default::default()
        };
        assert_eq!(provider.upstream_model("cheap"), "gpt-4o-mini");
        assert_eq!(provider.upstream_model("other"), "other");
    }

    #[test]
    fn validation_rejects_ambiguous_or_broken_providers() {
        let mut config = Config::example();
        config.validate().expect("the example config is valid");

        config.providers.push(Provider {
            name: "claude".into(),
            ..Default::default()
        });
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("used twice"));

        config.providers.pop();
        config.server.default_provider = Some("nope".into());
        assert!(config.validate().unwrap_err().to_string().contains("nope"));

        config.server.default_provider = None;
        config.providers[0].base_url = "api.anthropic.com".into();
        assert!(config.validate().unwrap_err().to_string().contains("http"));
    }

    #[test]
    fn json_round_trips_a_full_configuration() {
        let config = Config::example();
        let text = serde_json::to_string_pretty(&config).unwrap();
        let parsed: Config = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, config);
        assert_eq!(parsed.providers[0].patterns, vec!["claude*".to_string()]);
    }

    #[test]
    fn a_partial_json_inherits_defaults() {
        let parsed: Config = serde_json::from_str(
            r#"{
  "providers": [
    {
      "name": "local",
      "protocol": "openai",
      "base_url": "http://127.0.0.1:11434/v1"
    }
  ]
}"#,
        )
        .unwrap();

        assert_eq!(parsed.server.port, 8787);
        assert_eq!(parsed.server.default_max_tokens, 4096);
        assert!(parsed.providers[0].enabled);
        assert!(parsed.providers[0].models.is_empty());
    }
}
