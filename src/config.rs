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
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            idle_timeout_secs: default_idle_timeout(),
            max_sessions: default_max_sessions(),
            max_output_buffer: default_max_output_buffer(),
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

pub fn load_config(path: Option<&str>) -> anyhow::Result<Config> {
    match path {
        Some(p) => {
            let content = std::fs::read_to_string(p)?;
            Ok(toml::from_str(&content)?)
        }
        None => Ok(Config::default()),
    }
}
