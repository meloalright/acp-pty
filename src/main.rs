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

const HELP: &str = "\
shell-acp — a shell exposed as an Agent Client Protocol (ACP) agent

shell-acp is an ACP agent: an ACP client (e.g. cc-connect) spawns it as a
subprocess and speaks JSON-RPC 2.0 over stdio. It is not meant to be run
interactively in a terminal.

USAGE:
    shell-acp [--config <path>]

OPTIONS:
    --config <path>    Path to a TOML config (targets + session settings).
                       Omitted: sensible defaults (see config.example.toml).
    -h, --help         Print this help and exit.
    -V, --version      Print version and exit.

DOCS:
    https://github.com/meloalright/shell-acp";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP}");
        return;
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("shell-acp {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("shell_acp=info".parse().unwrap()),
        )
        .init();

    let config_path = std::env::args().skip_while(|a| a != "--config").nth(1);

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
