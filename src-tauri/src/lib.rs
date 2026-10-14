use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use proxy::{Config, LogBus, LogEntry, Probe, Provider, Proxy, Stats};

/// Everything the desktop shell owns: the live proxy, the saved configuration,
/// and one log bus that survives restarts so the panel keeps its history.
pub struct App {
    pub path: PathBuf,
    pub logs: LogBus,
    pub config: Mutex<Config>,
    pub proxy: Mutex<Option<Proxy>>,
    pub startup_error: Mutex<Option<String>>,
    /// Something the operator has to know about the file it started from, e.g.
    /// that it could not be parsed and a copy was kept aside.
    pub boot_warning: Mutex<Option<String>>,
}

/// One config location on every platform: `~/.llm_lite_proxy`. The per-platform
/// app-data directories made the file hard to find and hard to document.
fn config_dir(handle: &tauri::AppHandle) -> PathBuf {
    let home = handle.path().home_dir().expect("a home directory");
    home.join(".llm_lite_proxy")
}

/// Carries an existing file over so an upgrade does not look like a first run
/// with an empty config: the JSON file may still sit in the old app-data
/// directory, and the release before that stored TOML in either location.
fn migrate_legacy_config(handle: &tauri::AppHandle, path: &std::path::Path) {
    if path.exists() {
        return;
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(directory) = path.parent() {
        candidates.push(directory.join("config.toml"));
    }
    if let Ok(directory) = handle.path().app_config_dir() {
        candidates.push(directory.join("config.json"));
        candidates.push(directory.join("config.toml"));
    }

    for candidate in candidates {
        if !candidate.is_file() {
            continue;
        }
        let carried = match candidate.extension().and_then(|ext| ext.to_str()) {
            Some("toml") => std::fs::read_to_string(&candidate)
                .ok()
                .and_then(|text| toml::from_str::<Config>(&text).ok())
                .is_some_and(|config| config.save(path).is_ok()),
            _ => std::fs::copy(&candidate, path).is_ok(),
        };
        if carried {
            return;
        }
    }
}

/// Loads the configuration, and when the file cannot be parsed keeps a copy and
/// says so instead of silently starting over with the example config.
fn load_or_repair(path: &std::path::Path) -> (Config, Option<String>) {
    if !path.exists() {
        let example = Config::example();
        let _ = example.save(path);
        return (example, None);
    }
    match Config::load(path) {
        Ok(config) => (config, None),
        Err(error) => {
            let backup = path.with_extension("json.invalid");
            let _ = std::fs::copy(path, &backup);
            (
                Config::example(),
                Some(format!(
                    "配置文件无法解析（{error}），暂时用示例配置启动。原文件已备份到 {}，修好后重开即可恢复；现在保存会用界面上的内容覆盖它。",
                    backup.display()
                )),
            )
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub config: Config,
    pub running: bool,
    pub address: String,
    pub config_path: String,
    pub stats: Stats,
    pub error: Option<String>,
}

fn snapshot(app: &App) -> Snapshot {
    let config = app.config.lock().expect("config lock").clone();
    let (running, address) = match app.proxy.lock().expect("proxy lock").as_ref() {
        Some(proxy) => (proxy.listening(), proxy.addr().to_string()),
        None => (
            false,
            format!("{}:{}", config.server.host, config.server.port),
        ),
    };
    let startup = app.startup_error.lock().expect("error lock").clone();
    let boot = app.boot_warning.lock().expect("boot warning lock").clone();
    Snapshot {
        error: startup.or(boot),
        config,
        config_path: app.path.display().to_string(),
        running,
        address,
        stats: app.logs.stats(),
    }
}

async fn stop(app: &App) {
    let existing = app.proxy.lock().expect("proxy lock").take();
    if let Some(proxy) = existing {
        proxy.stop().await;
    }
}

async fn start(app: &App) -> Result<(), String> {
    let config = app.config.lock().expect("config lock").clone();
    config.validate().map_err(|error| error.to_string())?;

    let previous = app.proxy.lock().expect("proxy lock").take();
    if let Some(proxy) = previous {
        proxy.stop().await;
    }

    match Proxy::start_with(config, app.logs.clone()).await {
        Ok(proxy) => {
            *app.startup_error.lock().expect("error lock") = None;
            *app.proxy.lock().expect("proxy lock") = Some(proxy);
            Ok(())
        }
        Err(error) => {
            let message = error.message.clone();
            *app.startup_error.lock().expect("error lock") = Some(message.clone());
            Err(message)
        }
    }
}

/// Restarts only when the listening address changed; anything else is a hot
/// swap, so in-flight requests finish on the configuration they started with.
async fn apply_config(app: &App, config: Config) -> Result<(), String> {
    // Anything the listener or the router was built with needs a rebind; a
    // silent no-op here is what makes a settings change look ignored.
    let needs_rebind = {
        let current = app.config.lock().expect("config lock");
        current.server.host != config.server.host
            || current.server.port != config.server.port
            || current.server.max_body_mb != config.server.max_body_mb
    };
    app.logs.set_capacity(config.server.log_capacity);
    let running = app.proxy.lock().expect("proxy lock").is_some();
    *app.config.lock().expect("config lock") = config.clone();

    if needs_rebind && running {
        return start(app).await;
    }
    if running {
        let guard = app.proxy.lock().expect("proxy lock");
        if let Some(proxy) = guard.as_ref() {
            proxy.apply(Arc::new(config));
        }
        return Ok(());
    }
    Ok(())
}

#[tauri::command]
fn get_snapshot(app: State<'_, App>) -> Snapshot {
    snapshot(&app)
}

#[tauri::command]
async fn save_config(app: State<'_, App>, config: Config) -> Result<Snapshot, String> {
    config.validate().map_err(|error| error.to_string())?;
    let path = app.path.clone();
    let for_write = config.clone();
    tokio::task::spawn_blocking(move || for_write.save(&path))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error: proxy::ConfigError| error.to_string())?;

    apply_config(&app, config).await?;
    *app.boot_warning.lock().expect("boot warning lock") = None;
    Ok(snapshot(&app))
}

#[tauri::command]
async fn set_running(app: State<'_, App>, running: bool) -> Result<Snapshot, String> {
    if running {
        start(&app).await?;
    } else {
        stop(&app).await;
        *app.startup_error.lock().expect("error lock") = None;
    }
    Ok(snapshot(&app))
}

#[tauri::command]
async fn restart_proxy(app: State<'_, App>) -> Result<Snapshot, String> {
    start(&app).await?;
    Ok(snapshot(&app))
}

/// Probes the provider exactly as it is on screen, so an upstream that was just
/// added or renamed can be tested before it is saved.
#[tauri::command]
async fn test_provider(provider: Provider) -> Result<Probe, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .user_agent(concat!("llm-lite-proxy/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| error.to_string())?;
    Ok(proxy::upstream::probe(&client, &provider).await)
}

#[tauri::command]
fn recent_logs(app: State<'_, App>, limit: Option<usize>) -> Vec<LogEntry> {
    app.logs.recent(limit.unwrap_or(80))
}

#[tauri::command]
fn stats(app: State<'_, App>) -> Stats {
    app.logs.stats()
}

#[tauri::command]
fn open_config_path(app: State<'_, App>) -> String {
    app.path.display().to_string()
}

/// Paints the native title bar to match the panel. `null` hands the choice back
/// to the system, which is what an operator who never touched the toggle wants.
#[tauri::command]
fn set_window_theme(app: AppHandle, theme: Option<String>) -> Result<(), String> {
    let theme = match theme.as_deref() {
        None => None,
        Some("light") => Some(tauri::Theme::Light),
        Some("dark") => Some(tauri::Theme::Dark),
        Some(other) => return Err(format!("unknown theme `{other}`")),
    };
    let Some(window) = app.get_webview_window("main") else {
        return Ok(());
    };
    window.set_theme(theme).map_err(|error| error.to_string())
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();
            let directory = config_dir(&handle);
            std::fs::create_dir_all(&directory).ok();
            let path = directory.join("config.json");
            migrate_legacy_config(&handle, &path);

            let (config, warning) = load_or_repair(&path);
            if let Some(warning) = &warning {
                eprintln!("{warning}");
            }

            let logs = LogBus::new(config.server.log_capacity);
            app.manage(App {
                path: path.clone(),
                logs: logs.clone(),
                proxy: Mutex::new(None),
                config: Mutex::new(config),
                startup_error: Mutex::new(None),
                boot_warning: Mutex::new(warning),
            });

            let handle = app.handle().clone();
            let mut receiver = logs.subscribe();
            tauri::async_runtime::spawn(async move {
                loop {
                    match receiver.recv().await {
                        Ok(entry) => {
                            let _ = handle.emit("log-entry", entry);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });

            let boot_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = boot_handle.state::<App>();
                if let Err(error) = start(&state).await {
                    let _ = boot_handle.emit("proxy-error", error);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            save_config,
            set_running,
            restart_proxy,
            test_provider,
            recent_logs,
            stats,
            open_config_path,
            set_window_theme
        ])
        .run(tauri::generate_context!())
        .expect("the desktop app runs");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        listener.local_addr().expect("a bound address").port()
    }

    fn app_on(port: u16) -> App {
        let mut config = Config::example();
        config.server.host = "127.0.0.1".into();
        config.server.port = port;
        App {
            path: std::env::temp_dir().join("llm-lite-proxy-test.json"),
            logs: LogBus::new(64),
            config: Mutex::new(config),
            proxy: Mutex::new(None),
            startup_error: Mutex::new(None),
            boot_warning: Mutex::new(None),
        }
    }

    /// A rebind replaces the running proxy, so the state pointer is the reliable
    /// way to tell a restart from a hot swap when the port stays the same.
    fn proxy_identity(app: &App) -> Option<usize> {
        app.proxy
            .lock()
            .expect("proxy lock")
            .as_ref()
            .map(|proxy| Arc::as_ptr(&proxy.state) as usize)
    }

    fn live_port(app: &App) -> Option<u16> {
        app.proxy
            .lock()
            .expect("proxy lock")
            .as_ref()
            .map(|proxy| proxy.addr().port())
    }

    async fn health(port: u16) -> bool {
        reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/health"))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .map(|response| response.status().is_success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn a_port_change_rebinds_the_listener() {
        let first = free_port();
        let second = free_port();
        let app = app_on(first);
        start(&app).await.expect("the first start");
        assert_eq!(live_port(&app), Some(first));
        assert!(health(first).await);
        // Keep a pooled connection open, like a client or the GUI probe does.
        assert!(health(first).await);

        let mut next = app.config.lock().expect("config lock").clone();
        next.server.port = second;
        apply_config(&app, next).await.expect("the restart");

        assert_eq!(
            live_port(&app),
            Some(second),
            "the listener must move to the new port"
        );
        assert!(health(second).await, "the new port must answer");
        assert!(!health(first).await, "the old port must stop answering");
    }

    #[tokio::test]
    async fn a_hot_swap_does_not_rebind() {
        let port = free_port();
        let app = app_on(port);
        start(&app).await.expect("the first start");

        let mut next = app.config.lock().expect("config lock").clone();
        next.server.default_max_tokens = 1234;
        apply_config(&app, next).await.expect("the hot swap");

        assert_eq!(live_port(&app), Some(port));
        let live = app
            .proxy
            .lock()
            .expect("proxy lock")
            .as_ref()
            .expect("a running proxy")
            .state
            .config
            .load()
            .server
            .default_max_tokens;
        assert_eq!(live, 1234, "the live configuration must be swapped in");
    }

    #[tokio::test]
    async fn a_body_limit_change_rebinds_and_a_log_capacity_change_does_not() {
        let port = free_port();
        let app = app_on(port);
        start(&app).await.expect("the start");

        let before = proxy_identity(&app);

        let mut next = app.config.lock().expect("config lock").clone();
        next.server.log_capacity = 42;
        apply_config(&app, next)
            .await
            .expect("the log capacity change");
        assert_eq!(live_port(&app), Some(port), "log capacity must not rebind");
        assert_eq!(
            proxy_identity(&app),
            before,
            "log capacity is applied live, so the listener must survive"
        );
        assert_eq!(app.logs.stats().requests, 0);

        let mut bigger = app.config.lock().expect("config lock").clone();
        bigger.server.max_body_mb += 1;
        apply_config(&app, bigger)
            .await
            .expect("the body limit change");
        assert_ne!(
            proxy_identity(&app),
            before,
            "the body limit is baked into the router, so it needs a rebind"
        );
    }

    #[test]
    fn a_first_run_writes_an_example_config() {
        let dir = std::env::temp_dir().join("llm-lite-proxy-first-run");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let path = dir.join("config.json");

        let (config, warning) = load_or_repair(&path);
        assert!(warning.is_none());
        assert_eq!(config.providers.len(), 2);
        assert!(
            path.exists(),
            "the example is written so the file is findable"
        );
    }

    #[test]
    fn an_unreadable_config_is_backed_up_and_reported() {
        let dir = std::env::temp_dir().join("llm-lite-proxy-unreadable");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let path = dir.join("config.json");
        // A known field with the wrong type is what a hand edit usually breaks.
        let broken = r#"{"server": {"port": "8787"}, "providers": [{"name": "mine"}]}"#;
        std::fs::write(&path, broken).expect("a broken file");

        let (config, warning) = load_or_repair(&path);
        let warning = warning.expect("the problem is reported");
        assert!(warning.contains("json.invalid"), "{warning}");
        assert_eq!(
            config.providers.len(),
            2,
            "the example keeps the app usable"
        );

        let backup = path.with_extension("json.invalid");
        assert_eq!(
            std::fs::read_to_string(&backup).expect("the original is kept"),
            broken,
            "nothing is silently destroyed"
        );
    }

    #[tokio::test]
    async fn the_reported_port_is_the_bound_one() {
        let port = free_port();
        let app = app_on(port);
        start(&app).await.expect("the start");
        let shown = snapshot(&app).address;
        assert_eq!(shown, format!("127.0.0.1:{port}"));
    }
}
