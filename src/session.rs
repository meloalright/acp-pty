use crate::buffer::PromptTracker;
use crate::config::TargetConfig;
use anyhow::Result;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::Mutex;
use tokio::sync::mpsc;

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

        let mut cmd = CommandBuilder::new(&target.shell);
        if target.shell.ends_with("bash") {
            cmd.args(["--noprofile", "--norc"]);
        }
        cmd.cwd(&target.cwd);

        cmd.env("PATH", "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin");
        cmd.env("TERM", "xterm-256color");
        cmd.env("LANG", "en_US.UTF-8");
        cmd.env("HOME", target.cwd.to_string_lossy().to_string());

        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let writer = pair.master.take_writer()?;
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
            prompt_tracker: PromptTracker::new(),
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
