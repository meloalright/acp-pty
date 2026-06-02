use crate::buffer::PromptTracker;
use crate::config::TargetConfig;
use anyhow::Result;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::Mutex;
use tokio::sync::mpsc;

/// A per-session sentinel used as the shell prompt. Unique, alphanumeric, and
/// redacted from output, so prompt detection is exact regardless of the user's
/// theme and the marker is never shown to the user.
fn prompt_marker(session_id: &str) -> String {
    let token: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(12)
        .collect();
    format!("__SHELLACP_{}__", token)
}

pub struct LocalTerminalSession {
    pub session_id: String,
    pub target: TargetConfig,
    pub prompt_tracker: PromptTracker,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send>>,
    _reader_handle: std::thread::JoinHandle<()>,
}

impl LocalTerminalSession {
    pub fn spawn(
        session_id: String,
        target: &TargetConfig,
        byte_tx: mpsc::UnboundedSender<Vec<u8>>,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        // Launch the shell WITHOUT its rc/profile so no interactive first-run
        // step can hang us (notably zsh's `zsh-newuser-install` wizard when
        // there is no ~/.zshrc). We re-source the user's rc ourselves from the
        // init line below, then pin a deterministic sentinel prompt.
        let shell_name = target
            .shell
            .rsplit('/')
            .next()
            .unwrap_or(target.shell.as_str());
        let marker = prompt_marker(&session_id);

        let mut cmd = CommandBuilder::new(&target.shell);
        let init = if shell_name.contains("zsh") {
            cmd.args(["-f", "-i"]);
            format!(
                "[ -f \"$HOME/.zshrc\" ] && source \"$HOME/.zshrc\" >/dev/null 2>&1; \
                 precmd_functions=(); precmd() {{ :; }}; PS1='{m}'\n",
                m = marker
            )
        } else if shell_name.contains("bash") {
            cmd.args(["--norc", "--noprofile", "-i"]);
            format!(
                "[ -f \"$HOME/.bashrc\" ] && source \"$HOME/.bashrc\" >/dev/null 2>&1; \
                 PROMPT_COMMAND=''; PS1='{m}'\n",
                m = marker
            )
        } else {
            cmd.args(["-i"]);
            format!("PS1='{m}'\n", m = marker)
        };
        cmd.cwd(&target.cwd);

        for (key, val) in std::env::vars() {
            cmd.env(key, val);
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        cmd.env("HOME", &home);
        cmd.env("TERM", "xterm-256color");
        cmd.env("LANG", "en_US.UTF-8");

        let prompt_tracker = PromptTracker::new();
        prompt_tracker.set_marker(&marker);

        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let mut writer = pair.master.take_writer()?;
        writer.write_all(init.as_bytes())?;
        writer.flush()?;
        let mut reader = pair.master.try_clone_reader()?;

        let reader_handle = std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if byte_tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            session_id,
            target: target.clone(),
            prompt_tracker,
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            _reader_handle: reader_handle,
        })
    }

    pub fn write_to_pty(&self, data: &str) -> Result<()> {
        let mut w = self.writer.lock().map_err(|e| anyhow::anyhow!("{}", e))?;
        w.write_all(data.as_bytes())?;
        w.flush()?;
        Ok(())
    }

    pub fn send_signal(&self, sig: i32) -> Result<()> {
        let child = self.child.lock().map_err(|e| anyhow::anyhow!("{}", e))?;
        if let Some(pid) = child.process_id() {
            unsafe {
                libc::kill(pid as i32, sig);
            }
        }
        Ok(())
    }

    pub fn kill(&self) {
        if let Ok(mut child) = self.child.lock() {
            child.kill().ok();
        }
        self.prompt_tracker.force_complete();
    }
}
