use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitcherSettings {
    pub hotkey: String,
    pub group_by: String,
    pub preview_mode: String,
    #[serde(default = "default_window_mode")]
    pub window_mode: String,
    #[serde(default = "default_window_width")]
    pub window_width: u32,
    #[serde(default = "default_window_height")]
    pub window_height: u32,
    pub excluded_processes: Vec<String>,
}

fn default_window_mode() -> String {
    "fullscreen".to_string()
}

fn default_window_width() -> u32 {
    1280
}

fn default_window_height() -> u32 {
    720
}

impl Default for SwitcherSettings {
    fn default() -> Self {
        Self {
            hotkey: "Alt+`".to_string(),
            group_by: "exe_path".to_string(),
            preview_mode: "icons".to_string(),
            window_mode: default_window_mode(),
            window_width: default_window_width(),
            window_height: default_window_height(),
            excluded_processes: vec![
                "grouped-alt-tab.exe".to_string(),
                "ApplicationFrameHost.exe".to_string(),
            ],
        }
    }
}

fn settings_path(app: &AppHandle) -> anyhow::Result<PathBuf> {
    let dir = app
        .path()
        .app_config_dir()
        .context("failed to resolve app config directory")?;
    fs::create_dir_all(&dir).context("failed to create app config directory")?;
    Ok(dir.join("settings.json"))
}

pub fn load_settings(app: &AppHandle) -> anyhow::Result<SwitcherSettings> {
    let path = settings_path(app)?;
    if !path.exists() {
        return Ok(SwitcherSettings::default());
    }

    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn save_settings(app: &AppHandle, settings: &SwitcherSettings) -> anyhow::Result<()> {
    let path = settings_path(app)?;
    let raw = serde_json::to_string_pretty(settings).context("failed to serialize settings")?;
    fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::SwitcherSettings;

    #[test]
    fn default_settings_match_v1_scope() {
        let settings = SwitcherSettings::default();
        assert_eq!(settings.hotkey, "Alt+`");
        assert_eq!(settings.group_by, "exe_path");
        assert_eq!(settings.preview_mode, "icons");
        assert_eq!(settings.window_mode, "fullscreen");
        assert_eq!(settings.window_width, 1280);
        assert_eq!(settings.window_height, 720);
        assert!(settings
            .excluded_processes
            .contains(&"grouped-alt-tab.exe".to_string()));
    }
}
