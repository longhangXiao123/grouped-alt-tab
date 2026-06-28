mod settings;
mod window_manager;

use settings::{load_settings, save_settings, SwitcherSettings};
use tauri::{
    menu::MenuBuilder,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewWindow,
};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use window_manager::{activate_hwnd, cover_monitor, list_groups, AppGroup};

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

fn show_switcher_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };

    let _ = window.show();

    if let Err(error) = apply_window_bounds(app, &window) {
        let _ = app.emit("window:error", error.to_string());
    }

    let _ = window.set_focus();
}

fn apply_window_bounds(app: &AppHandle, window: &WebviewWindow) -> anyhow::Result<()> {
    let settings = load_settings(app)?;
    if settings.window_mode != "custom" {
        cover_monitor(window)?;
        return Ok(());
    }

    let monitor = window
        .current_monitor()?
        .or(window.primary_monitor()?)
        .ok_or_else(|| anyhow::anyhow!("no monitor available"))?;

    let monitor_position = *monitor.position();
    let monitor_size = *monitor.size();
    let size = PhysicalSize::new(
        settings.window_width.clamp(720, 7680),
        settings.window_height.clamp(460, 4320),
    );
    let x_offset = (monitor_size.width.saturating_sub(size.width) / 2) as i32;
    let y_offset = (monitor_size.height.saturating_sub(size.height) / 2) as i32;
    let position =
        PhysicalPosition::new(monitor_position.x + x_offset, monitor_position.y + y_offset);

    window.set_position(position)?;
    window.set_size(size)?;

    Ok(())
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

                    show_switcher_window(app);

                    let _ = app.emit("switcher:cycle", ());
                })
                .build(),
        )
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => {
                show_switcher_window(app);
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
                        show_switcher_window(app);
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
