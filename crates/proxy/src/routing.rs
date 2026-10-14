use crate::config::{matches_glob, Config, Provider};
use crate::error::ProxyError;

#[derive(Debug, PartialEq)]
pub struct Target<'a> {
    pub provider: &'a Provider,
    pub model: String,
}

pub fn resolve<'a>(config: &'a Config, requested: &str) -> Result<Target<'a>, ProxyError> {
    let requested = requested.trim();
    if requested.is_empty() {
        return Err(ProxyError::bad_request("field `model` must not be empty"));
    }

    let enabled: Vec<&Provider> = config
        .providers
        .iter()
        .filter(|provider| provider.enabled)
        .collect();

    if let Some(target) = by_provider_prefix(&enabled, requested) {
        return Ok(target);
    }
    if let Some(target) = by_alias(&enabled, requested) {
        return Ok(target);
    }
    if let Some(target) = by_pattern(&enabled, requested) {
        return Ok(target);
    }
    if let Some(name) = &config.server.default_provider {
        if let Some(provider) = enabled.iter().copied().find(|p| &p.name == name) {
            return Ok(Target {
                model: provider.upstream_model(requested),
                provider,
            });
        }
    }

    Err(ProxyError::not_found(format!(
        "no enabled provider matches model `{requested}`; configured providers: {}",
        if enabled.is_empty() {
            "none".to_string()
        } else {
            enabled
                .iter()
                .map(|provider| provider.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    )))
}

/// `provider/model` is explicit, so it wins over every other rule.
fn by_provider_prefix<'a>(providers: &[&'a Provider], requested: &str) -> Option<Target<'a>> {
    let (prefix, model) = requested.split_once('/')?;
    let provider = providers
        .iter()
        .copied()
        .find(|candidate| candidate.name.eq_ignore_ascii_case(prefix))?;
    Some(Target {
        model: provider.upstream_model(model),
        provider,
    })
}

fn by_alias<'a>(providers: &[&'a Provider], requested: &str) -> Option<Target<'a>> {
    let route = providers
        .iter()
        .flat_map(|provider| provider.models.iter().map(move |route| (provider, route)))
        .find(|(_, route)| route.public == requested)?
        .1;

    let provider = match &route.provider {
        Some(name) => providers
            .iter()
            .copied()
            .find(|candidate| &candidate.name == name)?,
        None => providers
            .iter()
            .copied()
            .find(|candidate| candidate.models.iter().any(|r| r.public == requested))?,
    };

    Some(Target {
        model: route.upstream.clone(),
        provider,
    })
}

fn by_pattern<'a>(providers: &[&'a Provider], requested: &str) -> Option<Target<'a>> {
    let provider = providers.iter().copied().find(|candidate| {
        candidate
            .patterns
            .iter()
            .any(|pattern| matches_glob(pattern, requested))
    })?;
    Some(Target {
        model: provider.upstream_model(requested),
        provider,
    })
}

pub fn public_models(config: &Config) -> Vec<(String, String)> {
    let mut names: Vec<(String, String)> = Vec::new();
    for provider in config.providers.iter().filter(|provider| provider.enabled) {
        if provider.models.is_empty() {
            continue;
        }
        for route in &provider.models {
            names.push((route.public.clone(), provider.name.clone()));
        }
    }
    names.sort_by(|left, right| left.0.cmp(&right.0));
    names.dedup_by(|left, right| left.0 == right.0);
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ModelRoute, Protocol, Server};
    use rstest::rstest;

    fn provider(
        name: &str,
        protocol: Protocol,
        patterns: &[&str],
        models: &[(&str, &str)],
    ) -> Provider {
        Provider {
            name: name.into(),
            protocol,
            base_url: protocol.default_base_url().into(),
            patterns: patterns.iter().map(|pattern| pattern.to_string()).collect(),
            models: models
                .iter()
                .map(|(public, upstream)| ModelRoute {
                    public: (*public).to_string(),
                    upstream: (*upstream).to_string(),
                    provider: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn config() -> Config {
        Config {
            server: Server::default(),
            providers: vec![
                provider(
                    "claude",
                    Protocol::Anthropic,
                    &["claude*"],
                    &[("cheap", "claude-haiku-4-5")],
                ),
                provider("gpt", Protocol::Openai, &["gpt*"], &[]),
                provider("router", Protocol::Openai, &["*"], &[]),
            ],
        }
    }

    #[rstest]
    #[case("claude-sonnet-4-5", "claude", "claude-sonnet-4-5")]
    #[case("gpt-4o", "gpt", "gpt-4o")]
    #[case("anything-else", "router", "anything-else")]
    #[case("cheap", "claude", "claude-haiku-4-5")]
    #[case("claude/claude-opus-4-5", "claude", "claude-opus-4-5")]
    #[case("gpt/gpt-4o", "gpt", "gpt-4o")]
    fn picks_the_expected_provider(
        #[case] requested: &str,
        #[case] name: &str,
        #[case] model: &str,
    ) {
        let config = config();
        let target = resolve(&config, requested).unwrap();
        assert_eq!(target.provider.name, name.to_string());
        assert_eq!(target.model, model.to_string());
    }

    #[test]
    fn a_disabled_provider_is_never_selected() {
        let mut config = config();
        config.providers[0].enabled = false;
        let target = resolve(&config, "claude-sonnet-4-5").unwrap();
        assert_eq!(target.provider.name, "router".to_string());
    }

    #[test]
    fn nothing_matches_when_every_provider_is_disabled() {
        let mut config = config();
        config.providers.clear();
        let error = resolve(&config, "claude").unwrap_err();
        assert_eq!(u16::from(error.status), 404);
        assert!(error.message.contains("none"));
    }

    #[test]
    fn the_default_provider_catches_models_no_pattern_claims() {
        let mut config = config();
        config
            .providers
            .retain(|provider| provider.name != "router");
        config.server.default_provider = Some("gpt".into());

        let target = resolve(&config, "llama-3.3").unwrap();
        assert_eq!(target.provider.name, "gpt".to_string());
    }

    #[test]
    fn an_empty_model_name_is_a_client_error() {
        let error = resolve(&config(), "   ").unwrap_err();
        assert_eq!(u16::from(error.status), 400);
    }

    #[test]
    fn a_route_can_point_one_alias_at_another_provider() {
        let mut config = config();
        config.providers[0].models[0].provider = Some("gpt".into());
        let target = resolve(&config, "cheap").unwrap();
        assert_eq!(target.provider.name, "gpt".to_string());
        assert_eq!(target.model, "claude-haiku-4-5".to_string());
    }

    #[test]
    fn the_model_listing_only_shows_enabled_aliases() {
        let mut config = config();
        config.providers[1].models.push(ModelRoute {
            public: "turbo".into(),
            upstream: "gpt-4o".into(),
            provider: None,
        });
        config.providers[2].enabled = false;

        let names = public_models(&config);
        assert_eq!(
            names,
            vec![
                ("cheap".to_string(), "claude".to_string()),
                ("turbo".to_string(), "gpt".to_string())
            ]
        );
    }
}
