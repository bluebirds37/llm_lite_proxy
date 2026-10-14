use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

pub mod config;
pub mod error;
pub mod logs;
pub mod routing;
pub mod server;
pub mod upstream;

pub use config::{matches_glob, Api, Config, ConfigError, ModelRoute, Protocol, Provider, Server};
pub use error::{Kind, ProxyError};
pub use logs::{LogBus, LogEntry, Stats};
pub use server::{
    build_state, build_state_with, endpoint_of, router, ProxyState, Shared, CHAT_ENDPOINT,
    MESSAGES_ENDPOINT, RESPONSES_ENDPOINT,
};

pub use server::Transformer;
pub use upstream::{forwarded_key, resolve_key, Probe};

/// A running proxy: the listener task, its address, and the shared state the
/// GUI reads and rewrites. Dropping `shutdown` closes the listener gracefully.
pub struct Proxy {
    pub state: Shared,
    addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl Proxy {
    pub async fn start(config: Config) -> Result<Self, ProxyError> {
        let capacity = config.server.log_capacity;
        Self::start_with(config, crate::logs::LogBus::new(capacity)).await
    }

    /// Lets a host keep one log bus across restarts so the panel keeps its
    /// history when the listening address changes.
    pub async fn start_with(config: Config, logs: LogBus) -> Result<Self, ProxyError> {
        let addr: SocketAddr = format!("{}:{}", config.server.host, config.server.port)
            .parse()
            .map_err(|error| {
                ProxyError::bad_request(format!("host and port do not form an address: {error}"))
            })?;
        let listener = TcpListener::bind(addr).await.map_err(|error| {
            ProxyError::new(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Kind::Internal,
                format!("could not listen on {addr}: {error}"),
            )
        })?;
        let bound = listener.local_addr().unwrap_or(addr);
        let state = build_state_with(config, logs);
        let (shutdown, waiter) = oneshot::channel::<()>();
        let state_task = Arc::clone(&state);

        let task = tokio::spawn(async move {
            let app = router(state_task);
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = waiter.await;
                })
                .await;
        });

        Ok(Self {
            state,
            addr: bound,
            shutdown: Some(shutdown),
            task: Some(task),
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn listening(&self) -> bool {
        self.task.as_ref().is_some_and(|task| !task.is_finished())
    }

    /// Swaps the live configuration. Requests already in flight keep the
    /// snapshot they started with.
    pub fn apply(&self, config: Arc<Config>) {
        self.state.config.store(config);
    }

    pub async fn stop(mut self) {
        drop(self.shutdown.take());
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        drop(self.shutdown.take());
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
