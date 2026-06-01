use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
    format!("{}\n\n... ({} bytes truncated) ...\n\n{}", head, text.len() - keep * 2, tail)
}

#[derive(Clone)]
pub struct PromptTracker {
    notify: Arc<Notify>,
    active: Arc<AtomicBool>,
    completed: Arc<Notify>,
}

impl PromptTracker {
    pub fn new() -> Self {
        Self {
            notify: Arc::new(Notify::new()),
            active: Arc::new(AtomicBool::new(false)),
            completed: Arc::new(Notify::new()),
        }
    }

    pub fn notify_output(&self) {
        if self.active.load(Ordering::Relaxed) {
            self.notify.notify_one();
        }
    }

    pub async fn wait_for_settle(&self, silence: Duration, hard_limit: Duration) {
        self.active.store(true, Ordering::Relaxed);
        let deadline = tokio::time::Instant::now() + hard_limit;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }

            let timeout = silence.min(remaining);

            tokio::select! {
                _ = tokio::time::sleep(timeout) => {
                    break;
                }
                _ = self.notify.notified() => {
                    continue;
                }
                _ = self.completed.notified() => {
                    break;
                }
            }
        }

        self.active.store(false, Ordering::Relaxed);
    }

    pub fn force_complete(&self) {
        self.active.store(false, Ordering::Relaxed);
        self.completed.notify_one();
    }
}
