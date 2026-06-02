use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Deserialize)]
pub struct Config {
    #[serde(default)]
    pub targets: HashMap<String, TargetConfig>,
    #[serde(default)]
    pub session: SessionConfig,
}

#[derive(Deserialize, Clone)]
pub struct TargetConfig {
    pub cwd: PathBuf,
    #[serde(default = "default_shell")]
    pub shell: String,
}

#[derive(Deserialize, Clone)]
pub struct SessionConfig {
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_secs: u64,
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
    #[serde(default = "default_max_output_buffer")]
    pub max_output_buffer: usize,
    /// How long the output must stay silent before a turn is considered
    /// finished when the shell prompt isn't recognized (e.g. inside a REPL
    /// like python3 that changes the prompt to `>>> `). Milliseconds.
    #[serde(default = "default_settle_idle_ms")]
    pub settle_idle_ms: u64,
    /// Absolute upper bound on how long a single turn may run before the
    /// prompt RPC is returned regardless. Seconds.
    #[serde(default = "default_settle_hard_limit_secs")]
    pub settle_hard_limit_secs: u64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            idle_timeout_secs: default_idle_timeout(),
            max_sessions: default_max_sessions(),
            max_output_buffer: default_max_output_buffer(),
            settle_idle_ms: default_settle_idle_ms(),
            settle_hard_limit_secs: default_settle_hard_limit_secs(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            targets: HashMap::new(),
            session: SessionConfig::default(),
        }
    }
}

fn default_shell() -> String {
    "bash".into()
}
fn default_idle_timeout() -> u64 {
    1800
}
fn default_max_sessions() -> usize {
    16
}
fn default_max_output_buffer() -> usize {
    65536
}
fn default_settle_idle_ms() -> u64 {
    800
}
fn default_settle_hard_limit_secs() -> u64 {
    120
}

pub fn load_config(path: Option<&str>) -> anyhow::Result<Config> {
    match path {
        Some(p) => {
            let content = std::fs::read_to_string(p)?;
            Ok(toml::from_str(&content)?)
        }
        None => Ok(Config::default()),
    }
}
