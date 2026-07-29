//! Listener binding and process lifecycle coordination.

use std::env;
use std::time::Duration;

use crate::core::{Result, ShuttleError};

use super::{router, GatewayListener};

pub async fn serve_listeners(listeners: Vec<GatewayListener>) -> Result<()> {
    if listeners.is_empty() {
        return Err(ShuttleError::Store(
            "at least one listener is required".to_owned(),
        ));
    }
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let mut tasks = tokio::task::JoinSet::new();
    for listener in listeners {
        let addr = listener.addr;
        let runtime = listener.runtime;
        let name = listener.name;
        let shutdown_rx = shutdown_rx.clone();
        tasks.spawn(async move {
            let tcp = tokio::net::TcpListener::bind(addr)
                .await
                .map_err(|err| ShuttleError::Store(format!("listener {name}: {err}")))?;
            axum::serve(tcp, router(runtime))
                .with_graceful_shutdown(async move {
                    let mut shutdown_rx = shutdown_rx;
                    while !*shutdown_rx.borrow() {
                        if shutdown_rx.changed().await.is_err() {
                            break;
                        }
                    }
                })
                .await
                .map_err(|err| ShuttleError::Store(format!("listener {name}: {err}")))
        });
    }

    let deadline = shutdown_deadline();
    let signal = wait_for_signal();
    tokio::pin!(signal);
    loop {
        tokio::select! {
            result = tasks.join_next() => {
                let Some(result) = result else { return Ok(()); };
                match result {
                    Ok(Ok(())) => {},
                    Ok(Err(error)) => {
                        let _ = shutdown_tx.send(true);
                        tasks.abort_all();
                        return Err(error);
                    }
                    Err(error) => {
                        let _ = shutdown_tx.send(true);
                        tasks.abort_all();
                        return Err(ShuttleError::Store(error.to_string()));
                    }
                }
            }
            _ = &mut signal => {
                let _ = shutdown_tx.send(true);
                let timeout = tokio::time::sleep(deadline);
                tokio::pin!(timeout);
                while !tasks.is_empty() {
                    tokio::select! {
                        _ = &mut timeout => {
                            tasks.abort_all();
                            return Ok(());
                        }
                        result = tasks.join_next() => {
                            if let Some(Err(error)) = result {
                                tasks.abort_all();
                                return Err(ShuttleError::Store(error.to_string()));
                            }
                        }
                    }
                }
                return Ok(());
            }
        }
    }
}

fn shutdown_deadline() -> Duration {
    env::var("SHUTTLE_SHUTDOWN_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| Duration::from_secs(seconds.clamp(1, 300)))
        .unwrap_or_else(|| Duration::from_secs(30))
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        let ctrl_c = tokio::signal::ctrl_c();
        let terminate = async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler")
                .recv()
                .await;
        };
        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
