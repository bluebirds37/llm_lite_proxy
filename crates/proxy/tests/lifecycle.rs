use std::time::Duration;

use proxy::{Config, LogBus, Proxy, Server};

fn config() -> Config {
    Config {
        server: Server {
            host: "127.0.0.1".into(),
            port: 0,
            ..Default::default()
        },
        providers: Vec::new(),
    }
}

fn address(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

#[tokio::test]
async fn stopping_does_not_wait_for_idle_keep_alive_connections() {
    let proxy = Proxy::start(config()).await.expect("the proxy binds");
    let port = proxy.addr().port();

    // A pooled client keeps its connection open after the request finishes,
    // which is exactly what a backend client or the GUI probe does.
    let client = reqwest::Client::new();
    let health = client
        .get(format!("{}/health", address(port)))
        .send()
        .await
        .expect("the proxy answers");
    assert_eq!(health.status(), 200);
    drop(health);

    let stopped = tokio::time::timeout(Duration::from_secs(2), proxy.stop()).await;
    assert!(
        stopped.is_ok(),
        "stop() waited for an idle keep-alive connection instead of closing the listener"
    );
}

#[tokio::test]
async fn restarting_moves_the_listener_to_the_new_port() {
    let logs = LogBus::new(64);
    let first = Proxy::start_with(config(), logs.clone())
        .await
        .expect("first bind");
    let first_port = first.addr().port();

    let client = reqwest::Client::new();
    client
        .get(format!("{}/health", address(first_port)))
        .send()
        .await
        .expect("the first listener answers");

    let mut moved = config();
    moved.server.port = 0;
    let second = tokio::time::timeout(Duration::from_secs(3), Proxy::start_with(moved, logs))
        .await
        .expect("restarting must not hang")
        .expect("the second bind succeeds");

    drop(first);
    let second_port = second.addr().port();
    assert_ne!(first_port, second_port);

    let health = client
        .get(format!("{}/health", address(second_port)))
        .send()
        .await
        .expect("the new listener answers");
    assert_eq!(health.status(), 200);
}

#[tokio::test]
async fn a_hot_swap_keeps_the_same_listener() {
    let proxy = Proxy::start(config()).await.expect("the proxy binds");
    let port = proxy.addr().port();

    let mut reloaded = config();
    reloaded.server.port = port;
    reloaded.server.default_max_tokens = 1234;
    proxy.apply(std::sync::Arc::new(reloaded.clone()));

    assert_eq!(
        proxy.state.config.load().server.default_max_tokens,
        1234,
        "a hot swap updates the live configuration without rebinding"
    );
    assert!(proxy.listening());
}
