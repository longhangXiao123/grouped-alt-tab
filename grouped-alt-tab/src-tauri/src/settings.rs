use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};
use tauri::{AppHandle, Manager};
use winreg::{enums::HKEY_CURRENT_USER, RegKey};

const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE_NAME: &str = "Grouped Alt-Tab";

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
    #[serde(default)]
    pub auto_start: bool,
    #[serde(default = "default_show_current_desktop_only")]
    pub show_current_desktop_only: bool,
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

fn default_show_current_desktop_only() -> bool {
    true
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
            auto_start: false,
            show_current_desktop_only: true,
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
        let mut settings = SwitcherSettings::default();
        settings.auto_start = is_auto_start_enabled()?;
        return Ok(settings);
    }

    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut settings: SwitcherSettings = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    settings.auto_start = is_auto_start_enabled()?;
    Ok(settings)
}

pub fn save_settings(app: &AppHandle, settings: &SwitcherSettings) -> anyhow::Result<()> {
    let path = settings_path(app)?;
    let raw = serde_json::to_string_pretty(settings).context("failed to serialize settings")?;
    fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))?;
    set_auto_start(settings.auto_start)
}

fn run_key() -> anyhow::Result<RegKey> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(RUN_KEY_PATH)
        .context("failed to open Windows startup registry key")?;
    Ok(key)
}

pub fn is_auto_start_enabled() -> anyhow::Result<bool> {
    let key = run_key()?;
    Ok(key.get_value::<String, _>(RUN_VALUE_NAME).is_ok())
}

pub fn set_auto_start(enabled: bool) -> anyhow::Result<()> {
    let key = run_key()?;
    if enabled {
        let exe = std::env::current_exe().context("failed to resolve current executable")?;
        let command = format!("\"{}\"", exe.display());
        key.set_value(RUN_VALUE_NAME, &command)
            .context("failed to enable startup launch")?;
    } else {
        match key.delete_value(RUN_VALUE_NAME) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("failed to disable startup launch"),
        }
    }

    Ok(())
}

pub fn update_auto_start(app: &AppHandle, enabled: bool) -> anyhow::Result<()> {
    let mut settings = load_settings(app)?;
    settings.auto_start = enabled;
    save_settings(app, &settings)
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
        assert!(!settings.auto_start);
        assert!(settings.show_current_desktop_only);
        assert!(settings
            .excluded_processes
            .contains(&"grouped-alt-tab.exe".to_string()));
    }
}
