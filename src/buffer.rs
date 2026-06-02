use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

pub fn strip_ansi(input: &[u8]) -> String {
    let stripped = strip_ansi_escapes::strip(input);
    String::from_utf8_lossy(&stripped).into_owned()
}

pub fn truncate_if_needed(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let keep = max_bytes / 2 - 20;
    let head = &text[..keep];
    let tail = &text[text.len() - keep..];
    format!(
        "{}\n\n... ({} bytes truncated) ...\n\n{}",
        head,
        text.len() - keep * 2,
        tail
    )
}

/// Last non-empty line of `text`, trimmed — used as a prompt fingerprint.
fn trailing_line(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|l| l.to_string())
}

fn ends_with(slot: &Mutex<Option<String>>, text: &str) -> bool {
    match &*slot.lock().unwrap() {
        Some(p) if !p.is_empty() => text.trim_end().ends_with(p.as_str()),
        _ => false,
    }
}

#[derive(Clone)]
pub struct PromptTracker {
    notify: Arc<Notify>,
    active: Arc<AtomicBool>,
    completed: Arc<Notify>,
    /// The shell prompt we match against — an injected sentinel marker (see
    /// `set_marker`). Deterministic regardless of theme / oh-my-zsh / colors.
    shell_prompt: Arc<Mutex<Option<String>>>,
    /// A prompt learned at runtime — the trailing line seen the last time a
    /// turn settled by silence. Lets nested REPLs (python3 `>>> `, `mysql>`,
    /// `node >`) settle instantly on the *second* command onward.
    learned_prompt: Arc<Mutex<Option<String>>>,
    /// Trailing line of the most recent flush, the candidate to be learned.
    last_trailing: Arc<Mutex<Option<String>>>,
    /// The sentinel string injected as PS1; redacted from displayed output.
    marker: Arc<Mutex<Option<String>>>,
    /// Set once the sentinel has first appeared (shell is up and integrated),
    /// or once the readiness watchdog gives up. Output before this is startup
    /// noise and is dropped.
    ready: Arc<AtomicBool>,
}

impl PromptTracker {
    pub fn new() -> Self {
        Self {
            notify: Arc::new(Notify::new()),
            active: Arc::new(AtomicBool::new(false)),
            completed: Arc::new(Notify::new()),
            shell_prompt: Arc::new(Mutex::new(None)),
            learned_prompt: Arc::new(Mutex::new(None)),
            last_trailing: Arc::new(Mutex::new(None)),
            marker: Arc::new(Mutex::new(None)),
            ready: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Register the injected sentinel as both the prompt to match and the
    /// string to redact from displayed output.
    pub fn set_marker(&self, marker: &str) {
        *self.shell_prompt.lock().unwrap() = Some(marker.to_string());
        *self.marker.lock().unwrap() = Some(marker.to_string());
    }

    pub fn contains_marker(&self, text: &str) -> bool {
        match &*self.marker.lock().unwrap() {
            Some(m) if !m.is_empty() => text.contains(m.as_str()),
            _ => false,
        }
    }

    /// Strip the sentinel from text before it is shown to the user.
    pub fn redact(&self, text: &str) -> String {
        match &*self.marker.lock().unwrap() {
            Some(m) if !m.is_empty() => text.replace(m.as_str(), ""),
            _ => text.to_string(),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    pub fn mark_ready(&self) {
        self.ready.store(true, Ordering::Relaxed);
    }

    /// Record the trailing line of a flush as the candidate prompt to learn.
    pub fn record_trailing(&self, text: &str) {
        if let Some(line) = trailing_line(text) {
            *self.last_trailing.lock().unwrap() = Some(line);
        }
    }

    pub fn ends_with_shell_prompt(&self, text: &str) -> bool {
        ends_with(&self.shell_prompt, text)
    }

    pub fn ends_with_learned_prompt(&self, text: &str) -> bool {
        ends_with(&self.learned_prompt, text)
    }

    /// Back at the shell prompt: forget any REPL prompt we had learned.
    pub fn clear_learned(&self) {
        *self.learned_prompt.lock().unwrap() = None;
    }

    pub fn notify_output(&self) {
        if self.active.load(Ordering::Relaxed) {
            self.notify.notify_one();
        }
    }

    /// Block until the current turn settles, then return so the prompt RPC
    /// can be answered. A turn ends when any of these happens first:
    ///   * a recognized prompt reappears — the shell prompt, or a REPL prompt
    ///     learned on a previous turn (`force_complete`); the fast path;
    ///   * the output stays silent for `idle_window` — fallback for prompts we
    ///     haven't learned yet. The window must exceed the cadence of a
    ///     continuous command (e.g. `ping` ~1s/line) so such streams keep the
    ///     turn alive and stream live instead of being cut off after one line;
    ///   * `hard_limit` elapses — absolute upper bound.
    ///
    /// When a turn settles by silence, the trailing line is promoted to the
    /// learned prompt so the next identical prompt settles via the fast path.
    pub async fn wait_for_settle(&self, idle_window: Duration, hard_limit: Duration) {
        self.active.store(true, Ordering::Relaxed);

        let deadline = tokio::time::Instant::now() + hard_limit;
        let mut settled_by_idle = false;

        loop {
            // Register interest before selecting so output that arrives while
            // we set up the select still wakes us (tokio::Notify holds one
            // permit for an un-awaited notify_one).
            let new_output = self.notify.notified();

            tokio::select! {
                _ = self.completed.notified() => break,
                _ = tokio::time::sleep_until(deadline) => break,
                _ = tokio::time::sleep(idle_window) => {
                    settled_by_idle = true;
                    break;
                }
                _ = new_output => {
                    // Fresh output: reset the idle timer and keep waiting.
                    continue;
                }
            }
        }

        self.active.store(false, Ordering::Relaxed);

        if settled_by_idle {
            let candidate = self.last_trailing.lock().unwrap().clone();
            if let Some(line) = candidate {
                if !line.is_empty() {
                    *self.learned_prompt.lock().unwrap() = Some(line);
                }
            }
        }
    }

    pub fn force_complete(&self) {
        self.active.store(false, Ordering::Relaxed);
        self.completed.notify_one();
    }
}
