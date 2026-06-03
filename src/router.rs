use crate::buffer::{truncate_if_needed, PromptTracker};
use crate::config::Config;
use crate::session::LocalTerminalSession;
use crate::target::resolve_target_or_default;
use crate::term::TermRenderer;
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
    CtrlD,
}

pub fn parse_command(text: &str) -> Option<Command> {
    let trimmed = text.trim();
    if trimmed.eq_ignore_ascii_case("@shell stop") {
        Some(Command::Stop)
    } else if trimmed.eq_ignore_ascii_case("@shell ctrl-c") {
        Some(Command::CtrlC)
    } else if trimmed.eq_ignore_ascii_case("@shell ctrl-d") {
        Some(Command::CtrlD)
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
            return Err(anyhow!(
                "max sessions ({}) reached",
                self.config.session.max_sessions
            ));
        }

        let target = resolve_target_or_default(&self.config, cwd);
        let (byte_tx, byte_rx) = mpsc::unbounded_channel::<Vec<u8>>();

        let session =
            LocalTerminalSession::spawn(session_id.to_string(), &target, byte_tx.clone())?;

        let prompt_tracker = session.prompt_tracker.clone();
        let stdout_tx = self.stdout_tx.clone();
        let sid = session_id.to_string();
        let sessions = self.sessions.clone();
        let max_buf = self.config.session.max_output_buffer;

        tokio::spawn(async move {
            output_read_loop(sid, byte_rx, prompt_tracker, stdout_tx, sessions, max_buf).await;
        });

        self.sessions
            .insert(session_id.to_string(), SessionState { session, byte_tx });

        Ok(())
    }

    pub fn has_session(&self, session_id: &str) -> bool {
        self.sessions.contains_key(session_id)
    }

    pub async fn handle_prompt(&self, session_id: &str, text: &str) -> Result<()> {
        match parse_command(text) {
            Some(Command::Stop) => {
                self.stop_session(session_id)?;
                send_block(&self.stdout_tx, session_id, "shell terminated");
                Ok(())
            }
            // Write the real control byte into the PTY, exactly like a terminal:
            // the tty line discipline delivers it to the foreground job (node,
            // python, …), not the shell. Ctrl-C (0x03) interrupts; Ctrl-D (0x04)
            // sends EOF, which exits a REPL/shell in one shot.
            Some(Command::CtrlC) => self.send_control(session_id, "\u{3}").await,
            Some(Command::CtrlD) => self.send_control(session_id, "\u{4}").await,
            None => {
                let prompt_tracker = {
                    let state = self
                        .sessions
                        .get(session_id)
                        .ok_or_else(|| anyhow!("session not found: {}", session_id))?;
                    state.session.prompt_tracker.clone()
                };
                // Wait for the shell to finish startup (sentinel seen) before
                // typing, so the first command's output isn't swallowed along
                // with the startup/init noise by the readiness gate.
                prompt_tracker
                    .wait_until_ready(Duration::from_secs(5))
                    .await;
                {
                    let state = self
                        .sessions
                        .get(session_id)
                        .ok_or_else(|| anyhow!("session not found: {}", session_id))?;
                    // Remember the command so its terminal echo is stripped from
                    // the output the user sees (they already typed it).
                    prompt_tracker.set_pending_echo(text);
                    let input = format!("{}\n", text);
                    state.session.write_to_pty(&input)?;
                }
                prompt_tracker
                    .wait_for_settle(
                        Duration::from_millis(self.config.session.settle_idle_ms),
                        Duration::from_secs(self.config.session.settle_hard_limit_secs),
                    )
                    .await;
                // Close the turn's code fence (opened in emit_step), regardless
                // of how it settled (prompt match / idle / hard limit).
                if prompt_tracker.take_fence_open() {
                    send_text(&self.stdout_tx, session_id, "```");
                }
                Ok(())
            }
        }
    }

    /// Write a raw control byte to the PTY (e.g. Ctrl-C/Ctrl-D), then let the
    /// resulting output settle and close the turn's fence.
    async fn send_control(&self, session_id: &str, bytes: &str) -> Result<()> {
        let prompt_tracker = {
            let state = self
                .sessions
                .get(session_id)
                .ok_or_else(|| anyhow!("session not found: {}", session_id))?;
            state.session.write_to_pty(bytes)?;
            state.session.prompt_tracker.clone()
        };
        prompt_tracker
            .wait_for_settle(
                Duration::from_millis(self.config.session.settle_idle_ms),
                Duration::from_secs(self.config.session.settle_hard_limit_secs),
            )
            .await;
        if prompt_tracker.take_fence_open() {
            send_text(&self.stdout_tx, session_id, "```");
        }
        Ok(())
    }

    pub fn stop_session(&self, session_id: &str) -> Result<()> {
        let (_, state) = self
            .sessions
            .remove(session_id)
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

/// Send a raw `agent_message_chunk`. cc-connect accumulates a turn's chunks
/// into one message, so a turn's output forms a single fenced code block by
/// opening the fence on the first chunk and closing it at settle (see
/// `emit_step`) — rather than fencing every chunk, which stacks `` ``` ``.
fn send_text(stdout_tx: &StdoutTx, session_id: &str, text: &str) {
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

/// Send a standalone one-off message as its own fenced block (status notices
/// like "SIGINT sent", "shell exited").
fn send_block(stdout_tx: &StdoutTx, session_id: &str, text: &str) {
    send_text(
        stdout_tx,
        session_id,
        &format!("```\n{}\n```", text.trim_end()),
    );
}

/// Render newly-finalized terminal rows, emit them to the user, and run
/// prompt/settle detection against the active (cursor) line.
fn emit_step(
    renderer: &mut TermRenderer,
    prompt_tracker: &PromptTracker,
    stdout_tx: &StdoutTx,
    session_id: &str,
    max_output_buffer: usize,
    alt_notified: &mut bool,
) {
    // Startup gate: drop everything until the sentinel prompt first renders, so
    // the shell coming up + our init line are never shown. The watchdog also
    // flips readiness if the marker never appears.
    if !prompt_tracker.is_ready() {
        if prompt_tracker.contains_marker(&renderer.current_line()) {
            prompt_tracker.mark_ready();
            renderer.skip_to_cursor();
        }
        return;
    }

    // Full-screen TUI (alternate screen): can't be shown over chat — it redraws
    // a fixed grid forever and never settles. Notify once, end the turn, and let
    // the user exit. Don't stream the redraw garbage.
    if renderer.in_alt_screen() {
        if !*alt_notified {
            *alt_notified = true;
            if prompt_tracker.take_fence_open() {
                send_text(stdout_tx, session_id, "```");
            }
            send_text(
                stdout_tx,
                session_id,
                "⚠️ 这个程序启动了全屏界面(TUI),无法在聊天里呈现。\n请用它的非交互模式(如 `opencode run \"...\"`),或发 `@shell ctrl-c` / `@shell ctrl-d` / `@shell stop` 退出。",
            );
            // End this turn exactly once. Calling it on every alt-screen tick
            // would leave stale `completed` permits that settle the next real
            // turn prematurely.
            prompt_tracker.force_complete();
        }
        return;
    }
    *alt_notified = false;

    // Finalized rows above the cursor are the command output. Redact the
    // sentinel (in case the command line itself carried the prompt) and strip
    // the command's own echo, then send.
    let text = renderer.take_completed();
    if !text.is_empty() {
        let display = prompt_tracker.redact(&text);
        let display = prompt_tracker.strip_pending_echo(&display);
        if !display.trim().is_empty() {
            let display = truncate_if_needed(&display, max_output_buffer);
            // First chunk of the turn opens the code fence; subsequent chunks
            // are raw, so cc-connect accumulates them into one block.
            if prompt_tracker.open_fence() {
                send_text(stdout_tx, session_id, &format!("```\n{}", display));
            } else {
                send_text(stdout_tx, session_id, &display);
            }
        }
    }

    // Settle detection runs on the active line — the prompt that's currently
    // displayed (never emitted as output).
    let line = renderer.current_line();
    if prompt_tracker.ends_with_shell_prompt(&line) {
        prompt_tracker.clear_learned();
        prompt_tracker.force_complete();
    } else if prompt_tracker.ends_with_learned_prompt(&line) {
        prompt_tracker.force_complete();
    } else {
        // Unknown active line (e.g. a REPL prompt): remember it to learn on an
        // idle settle, and reset the idle timer when there's fresh activity.
        if !line.is_empty() {
            prompt_tracker.record_trailing(&line);
        }
        if !text.is_empty() || !line.is_empty() {
            prompt_tracker.notify_output();
        }
    }
}

async fn output_read_loop(
    session_id: String,
    mut byte_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    prompt_tracker: PromptTracker,
    stdout_tx: StdoutTx,
    sessions: Arc<DashMap<SessionId, SessionState>>,
    max_output_buffer: usize,
) {
    let mut renderer = TermRenderer::new();
    let mut alt_notified = false;
    let flush_interval = Duration::from_millis(100);

    // Readiness watchdog: if the sentinel never appears (integration failed, or
    // the shell is genuinely stuck at an interactive prompt), give up waiting
    // after a few seconds so output is shown rather than silently swallowed.
    let watchdog = prompt_tracker.clone();
    let watchdog_sid = session_id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        if !watchdog.is_ready() {
            tracing::warn!(
                session_id = %watchdog_sid,
                "prompt marker not seen within 5s; shell may be stuck at an interactive prompt — showing raw output"
            );
            watchdog.mark_ready();
        }
    });

    loop {
        tokio::select! {
            bytes = byte_rx.recv() => {
                match bytes {
                    Some(data) => {
                        renderer.feed(&data);
                    }
                    None => {
                        emit_step(&mut renderer, &prompt_tracker, &stdout_tx, &session_id, max_output_buffer, &mut alt_notified);
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(flush_interval), if renderer.pending() || renderer.in_alt_screen() || !prompt_tracker.is_ready() => {
                emit_step(&mut renderer, &prompt_tracker, &stdout_tx, &session_id, max_output_buffer, &mut alt_notified);
            }
        }
    }

    sessions.remove(&session_id);
    // If the shell died mid-turn with an open fence, close it first.
    if prompt_tracker.take_fence_open() {
        send_text(&stdout_tx, &session_id, "```");
    }
    send_block(&stdout_tx, &session_id, "shell exited");
    prompt_tracker.force_complete();
}
