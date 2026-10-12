//! TOML preferences and XDG paths. No GTK dependency in the data model.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Apps,
    Clipboard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub shortcut: String,
    pub default_mode: Mode,
    pub theme: Theme,
    pub max_results: usize,
    pub recent_apps_limit: usize,
    pub history_limit: usize,
    pub recording: bool,
    pub persist_history: bool,
    pub autostart: bool,
    pub recent_apps: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shortcut: "<Alt>space".into(),
            default_mode: Mode::Apps,
            theme: Theme::System,
            max_results: 8,
            recent_apps_limit: 5,
            history_limit: 100,
            recording: true,
            persist_history: true,
            autostart: true,
            recent_apps: vec![],
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read_to_string(path).context("无法读取偏好设置")?;
        let config: Self = toml::from_str(&data).context("偏好设置 TOML 格式错误")?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if !(4..=12).contains(&self.max_results) {
            bail!("剪贴板显示条数必须在 4 到 12 之间");
        }
        if self.recent_apps_limit > 20 {
            bail!("最近使用显示条数必须在 0 到 20 之间");
        }
        if !(10..=500).contains(&self.history_limit) {
            bail!("历史条数必须在 10 到 500 之间");
        }
        if self.shortcut.is_empty() || self.shortcut.len() > 100 {
            bail!("快捷键不能为空");
        }
        Ok(())
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        atomic_write(path, toml::to_string_pretty(self)?.as_bytes())
    }
}

pub fn xdg_path(variable: &str, fallback: &str) -> PathBuf {
    env::var_os(variable)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(fallback))
        .join("quick-launcher")
}

pub fn config_path() -> PathBuf {
    xdg_path("XDG_CONFIG_HOME", ".config").join("config.toml")
}
pub fn state_dir() -> PathBuf {
    xdg_path("XDG_STATE_HOME", ".local/state")
}

pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let directory = path.parent().context("存储路径没有父目录")?;
    fs::create_dir_all(directory)?;
    let temporary = path.with_extension("tmp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(data)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_configuration_gets_new_defaults() {
        let config: Config = toml::from_str("max_results = 6").unwrap();
        assert_eq!(config.max_results, 6);
        assert_eq!(config.shortcut, "<Alt>space");
        assert!(config.recording);
        assert_eq!(config.recent_apps_limit, 5);
    }
    #[test]
    fn config_round_trips_and_rejects_invalid_limits() {
        let mut config = Config::default();
        assert_eq!(
            config,
            toml::from_str::<Config>(&toml::to_string(&config).unwrap()).unwrap()
        );
        config.max_results = 1000;
        assert!(config.validate().is_err());
        config.max_results = 8;
        config.recent_apps_limit = 0;
        assert!(config.validate().is_ok());
        config.recent_apps_limit = 21;
        assert!(config.validate().is_err());
    }
}
