use crate::config::{Config, TargetConfig};
use std::path::Path;

pub fn resolve_target<'a>(config: &'a Config, cwd: &str) -> Option<(String, &'a TargetConfig)> {
    let cwd_path = Path::new(cwd);
    for (name, target) in &config.targets {
        if target.cwd == cwd_path {
            return Some((name.clone(), target));
        }
    }
    None
}

pub fn resolve_target_or_default(config: &Config, cwd: &str) -> TargetConfig {
    match resolve_target(config, cwd) {
        Some((_, target)) => target.clone(),
        None => TargetConfig {
            cwd: cwd.into(),
            shell: default_shell_for_platform(),
        },
    }
}

fn default_shell_for_platform() -> String {
    if Path::new("/bin/zsh").exists() {
        "zsh".into()
    } else {
        "bash".into()
    }
}
