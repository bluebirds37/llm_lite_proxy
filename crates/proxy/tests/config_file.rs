use std::fs;
use std::path::PathBuf;

use proxy::{Api, Config, ModelRoute, Protocol, Provider};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("llm-lite-proxy-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("a scratch directory");
    dir.join("config.json")
}

fn provider(name: &str) -> Provider {
    Provider {
        name: name.into(),
        protocol: Protocol::Openai,
        api: Some(Api::Chat),
        base_url: "https://openrouter.ai/api/v1".into(),
        api_key: "sk-test".into(),
        enabled: true,
        patterns: vec!["claude*".into()],
        models: vec![ModelRoute {
            public: "fast".into(),
            upstream: "anthropic/claude-3.5".into(),
            provider: None,
        }],
        headers: Default::default(),
    }
}

#[test]
fn saving_over_an_existing_file_keeps_the_newest_content() {
    let path = scratch("save-twice");

    let mut config = Config::example();
    config.save(&path).expect("the first save");

    config.providers.push(provider("open_router"));
    config.server.passthrough_unknown_params = true;
    config
        .save(&path)
        .expect("saving over an existing file must not fail");

    let reloaded = Config::load(&path).expect("the saved file parses");
    assert_eq!(
        reloaded.providers.len(),
        3,
        "the added provider must survive"
    );
    let added = reloaded
        .providers
        .iter()
        .find(|provider| provider.name == "open_router")
        .expect("the added provider is present");
    assert_eq!(added.api_key, "sk-test".to_string());
    assert_eq!(added.models[0].public, "fast".to_string());
    assert_eq!(added.base_url, "https://openrouter.ai/api/v1".to_string());
    assert!(reloaded.server.passthrough_unknown_params);
}

#[test]
fn a_removed_provider_is_not_resurrected() {
    let path = scratch("remove");

    let mut config = Config::example();
    config.save(&path).expect("the first save");
    config
        .providers
        .retain(|provider| provider.name != "openai");
    config.save(&path).expect("the second save");

    let reloaded = Config::load(&path).expect("the saved file parses");
    assert_eq!(reloaded.providers.len(), 1);
    assert_eq!(reloaded.providers[0].name, "claude".to_string());
}

#[test]
fn no_temporary_file_is_left_behind() {
    let path = scratch("temp-file");
    Config::example().save(&path).expect("the save");

    let leftovers: Vec<String> = fs::read_dir(path.parent().expect("a parent"))
        .expect("the directory reads")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
}

#[test]
fn a_broken_file_falls_back_without_throwing() {
    let path = scratch("broken");
    fs::write(&path, "{\"providers\": [").expect("a broken file");

    let error = Config::load(&path).expect_err("a broken file is reported");
    assert!(error.to_string().contains("config.json"), "{error}");
}
