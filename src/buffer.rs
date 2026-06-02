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

#[derive(Clone)]
pub struct PromptTracker {
    notify: Arc<Notify>,
    active: Arc<AtomicBool>,
    completed: Arc<Notify>,
    shell_prompt: Arc<Mutex<Option<String>>>,
}

impl PromptTracker {
    pub fn new() -> Self {
        Self {
            notify: Arc::new(Notify::new()),
            active: Arc::new(AtomicBool::new(false)),
            completed: Arc::new(Notify::new()),
            shell_prompt: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_shell_prompt(&self, prompt: &str) {
        let trimmed = prompt.trim().to_string();
        if !trimmed.is_empty() {
            *self.shell_prompt.lock().unwrap() = Some(trimmed);
        }
    }

    pub fn output_ends_with_prompt(&self, text: &str) -> bool {
        if let Some(prompt) = &*self.shell_prompt.lock().unwrap() {
            text.trim_end().ends_with(prompt.as_str())
        } else {
            false
        }
    }

    pub fn notify_output(&self) {
        if self.active.load(Ordering::Relaxed) {
            self.notify.notify_one();
        }
    }

    pub async fn wait_for_settle(&self, hard_limit: Duration) {
        self.active.store(true, Ordering::Relaxed);

        tokio::select! {
            _ = self.completed.notified() => {}
            _ = tokio::time::sleep(hard_limit) => {}
        }

        self.active.store(false, Ordering::Relaxed);
    }

    pub fn force_complete(&self) {
        self.active.store(false, Ordering::Relaxed);
        self.completed.notify_one();
    }
}
