use crate::buffer::{strip_ansi, truncate_if_needed, PromptTracker};
use crate::config::Config;
use crate::session::LocalTerminalSession;
use crate::target::resolve_target_or_default;
use anyhow::{anyhow, Result};
use dashmap::DashMap;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub type StdoutTx = mpsc::UnboundedSender<String>;
pub type SessionId = String;

pub struct SessionRouter {
    sessions: Arc<DashMap<SessionId, SessionState>>,
    config: Arc<Config>,
    stdout_tx: StdoutTx,
}

struct SessionState {
    session: LocalTerminalSession,
    byte_tx: mpsc::UnboundedSender<Vec<u8>>,
}

pub enum Command {
    Stop,
    CtrlC,
}

pub fn parse_command(text: &str) -> Option<Command> {
    let trimmed = text.trim();
    if trimmed.eq_ignore_ascii_case("@term stop") {
        Some(Command::Stop)
    } else if trimmed.eq_ignore_ascii_case("@term ctrl-c") {
        Some(Command::CtrlC)
    } else {
        None
    }
}

impl SessionRouter {
    pub fn new(config: Arc<Config>, stdout_tx: StdoutTx) -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
            config,
            stdout_tx,
        }
    }

    pub fn create_session(&self, session_id: &str, cwd: &str) -> Result<()> {
        if self.sessions.len() >= self.config.session.max_sessions {
            return Err(anyhow!("max sessions ({}) reached", self.config.session.max_sessions));
        }

        let target = resolve_target_or_default(&self.config, cwd);
        let (byte_tx, byte_rx) = mpsc::unbounded_channel::<Vec<u8>>();

        let session = LocalTerminalSession::spawn(session_id.to_string(), &target, byte_tx.clone())?;

        let prompt_tracker = session.prompt_tracker.clone();
        let stdout_tx = self.stdout_tx.clone();
        let sid = session_id.to_string();
        let sessions = self.sessions.clone();
        let max_buf = self.config.session.max_output_buffer;

        tokio::spawn(async move {
            output_read_loop(sid, byte_rx, prompt_tracker, stdout_tx, sessions, max_buf).await;
        });

        self.sessions.insert(
            session_id.to_string(),
            SessionState { session, byte_tx },
        );

        Ok(())
    }

    pub fn has_session(&self, session_id: &str) -> bool {
        self.sessions.contains_key(session_id)
    }

    pub async fn handle_prompt(&self, session_id: &str, text: &str) -> Result<()> {
        match parse_command(text) {
            Some(Command::Stop) => {
                self.stop_session(session_id)?;
                send_update(&self.stdout_tx, session_id, "shell terminated");
                Ok(())
            }
            Some(Command::CtrlC) => {
                let state = self.sessions.get(session_id)
                    .ok_or_else(|| anyhow!("session not found: {}", session_id))?;
                state.session.send_signal(2)?; // SIGINT
                send_update(&self.stdout_tx, session_id, "SIGINT sent");
                Ok(())
            }
            None => {
                let prompt_tracker = {
                    let state = self.sessions.get(session_id)
                        .ok_or_else(|| anyhow!("session not found: {}", session_id))?;
                    let input = format!("{}\n", text);
                    state.session.write_to_pty(&input)?;
                    state.session.prompt_tracker.clone()
                };
                prompt_tracker
                    .wait_for_settle(
                        Duration::from_millis(300),
                        Duration::from_secs(30),
                    )
                    .await;
                Ok(())
            }
        }
    }

    pub fn stop_session(&self, session_id: &str) -> Result<()> {
        let (_, state) = self.sessions.remove(session_id)
            .ok_or_else(|| anyhow!("session not found: {}", session_id))?;
        state.session.kill();
        Ok(())
    }

    pub fn list_sessions(&self) -> Vec<serde_json::Value> {
        self.sessions
            .iter()
            .map(|entry| {
                json!({
                    "sessionId": entry.key(),
                    "cwd": entry.value().session.target.cwd.to_string_lossy(),
                    "title": entry.value().session.target.shell,
                })
            })
            .collect()
    }
}

fn send_update(stdout_tx: &StdoutTx, session_id: &str, text: &str) {
    let msg = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": text}
            }
        }
    });
    stdout_tx.send(msg.to_string()).ok();
}

async fn output_read_loop(
    session_id: String,
    mut byte_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    prompt_tracker: PromptTracker,
    stdout_tx: StdoutTx,
    sessions: Arc<DashMap<SessionId, SessionState>>,
    max_output_buffer: usize,
) {
    let mut buffer: Vec<u8> = Vec::new();
    let flush_interval = Duration::from_millis(300);
    let max_chunk = 4096usize;

    loop {
        tokio::select! {
            bytes = byte_rx.recv() => {
                match bytes {
                    Some(data) => {
                        buffer.extend_from_slice(&data);
                        if buffer.len() >= max_chunk {
                            let chunk = buffer.drain(..).collect::<Vec<_>>();
                            let text = strip_ansi(&chunk);
                            let text = truncate_if_needed(&text, max_output_buffer);
                            send_update(&stdout_tx, &session_id, &text);
                            prompt_tracker.notify_output();
                        }
                    }
                    None => {
                        // PTY reader closed
                        if !buffer.is_empty() {
                            let text = strip_ansi(&buffer);
                            let text = truncate_if_needed(&text, max_output_buffer);
                            send_update(&stdout_tx, &session_id, &text);
                            prompt_tracker.notify_output();
                        }
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(flush_interval), if !buffer.is_empty() => {
                let chunk = buffer.drain(..).collect::<Vec<_>>();
                let text = strip_ansi(&chunk);
                let text = truncate_if_needed(&text, max_output_buffer);
                send_update(&stdout_tx, &session_id, &text);
                prompt_tracker.notify_output();
            }
        }
    }

    sessions.remove(&session_id);
    send_update(&stdout_tx, &session_id, "shell exited");
    prompt_tracker.force_complete();
}
