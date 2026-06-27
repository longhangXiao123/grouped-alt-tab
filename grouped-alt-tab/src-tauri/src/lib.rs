mod settings;
mod window_manager;

use settings::{load_settings, save_settings, SwitcherSettings};
use tauri::{
    menu::MenuBuilder,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager,
};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use window_manager::{activate_hwnd, list_groups, AppGroup};

#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error("{0}")]
    Message(String),
}

impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl From<anyhow::Error> for AppError {
    fn from(value: anyhow::Error) -> Self {
        Self::Message(value.to_string())
    }
}

type AppResult<T> = Result<T, AppError>;

#[tauri::command]
fn list_window_groups(app: AppHandle) -> AppResult<Vec<AppGroup>> {
    list_groups(&app).map_err(AppError::from)
}

#[tauri::command]
fn activate_window(hwnd: String) -> AppResult<()> {
    activate_hwnd(hwnd).map_err(AppError::from)
}

#[tauri::command]
fn get_settings(app: AppHandle) -> AppResult<SwitcherSettings> {
    load_settings(&app).map_err(AppError::from)
}

#[tauri::command]
fn update_settings(app: AppHandle, settings: SwitcherSettings) -> AppResult<()> {
    save_settings(&app, &settings).map_err(AppError::from)
}

pub fn run() {
    let handler_shortcut = Shortcut::new(Some(Modifiers::ALT), Code::Backquote);
    let register_shortcut = Shortcut::new(Some(Modifiers::ALT), Code::Backquote);

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, event_shortcut, event| {
                    if event_shortcut != &handler_shortcut
                        || event.state() != ShortcutState::Pressed
                    {
                        return;
                    }

                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }

                    let _ = app.emit("switcher:cycle", ());
                })
                .build(),
        )
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
                let _ = app.emit("switcher:open", ());
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            list_window_groups,
            activate_window,
            get_settings,
            update_settings
        ])
        .setup(move |app| {
            if let Err(error) = app.global_shortcut().register(register_shortcut) {
                app.handle()
                    .emit("hotkey:error", error.to_string())
                    .unwrap_or_default();
            }

            let menu = MenuBuilder::new(app)
                .text("show", "Show Switcher")
                .separator()
                .text("quit", "Exit")
                .build()?;

            let tray = TrayIconBuilder::with_id("main")
                .icon(
                    app.default_window_icon()
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("default app icon is missing"))?,
                )
                .tooltip("Grouped Alt-Tab")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                        let _ = app.emit("switcher:open", ());
                    }
                })
                .build(app)?;

            let _ = app.manage(tray);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to run app");
}
