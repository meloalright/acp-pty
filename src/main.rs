mod acp;
mod buffer;
mod config;
mod router;
mod session;
mod target;

use crate::acp::RpcRequest;
use crate::router::SessionRouter;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("shell_acp=info".parse().unwrap()),
        )
        .init();

    let config_path = std::env::args()
        .skip_while(|a| a != "--config")
        .nth(1);

    let config = match config::load_config(config_path.as_deref()) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            tracing::error!(error = %e, "failed to load config");
            std::process::exit(1);
        }
    };

    let (stdout_tx, mut stdout_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = stdout_rx.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if stdout.write_all(b"\n").await.is_err() {
                break;
            }
            stdout.flush().await.ok();
        }
    });

    let router = Arc::new(SessionRouter::new(config, stdout_tx.clone()));

    tracing::info!("shell-acp started");

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<RpcRequest>(trimmed) {
                    Ok(req) => {
                        acp::handle_request(req, &router, &stdout_tx).await;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, line = %trimmed, "invalid JSON-RPC request");
                    }
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "stdin read error");
                break;
            }
        }
    }

    tracing::info!("shell-acp shutting down");
}
