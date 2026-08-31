mod settings;
mod window_manager;

use settings::{
    is_auto_start_enabled, load_settings, save_settings, update_auto_start, SwitcherSettings,
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;
use tauri::{
    menu::{CheckMenuItemBuilder, MenuBuilder},
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

const TRAY_ID: &str = "main";

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
    save_settings(&app, &settings).map_err(AppError::from)?;
    refresh_tray_menu(&app);
    Ok(())
}

#[tauri::command]
fn apply_switcher_window_bounds(app: AppHandle) -> AppResult<()> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| AppError::Message("main window is missing".to_string()))?;
    apply_window_bounds(&app, &window).map_err(AppError::from)
}

fn build_tray_menu(app: &AppHandle) -> anyhow::Result<tauri::menu::Menu<tauri::Wry>> {
    let auto_start = is_auto_start_enabled()?;
    let auto_start_item = CheckMenuItemBuilder::with_id("auto_start", "Start with Windows")
        .checked(auto_start)
        .build(app)?;

    MenuBuilder::new(app)
        .text("show", "Show Switcher")
        .item(&auto_start_item)
        .separator()
        .text("quit", "Exit")
        .build()
        .map_err(Into::into)
}

fn refresh_tray_menu(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };

    if let Ok(menu) = build_tray_menu(app) {
        let _ = tray.set_menu(Some(menu));
    }
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
        window.set_fullscreen(true)?;
        cover_monitor(window)?;
        return Ok(());
    }

    if window.is_fullscreen()? {
        window.set_fullscreen(false)?;
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
    let group_handler_shortcut =
        Shortcut::new(Some(Modifiers::ALT | Modifiers::SHIFT), Code::Backquote);
    let group_register_shortcut = group_handler_shortcut;
    let repeat_generation = Arc::new(AtomicU64::new(0));
    let group_repeat_generation = Arc::new(AtomicU64::new(0));
    let repeat_generation_handler = Arc::clone(&repeat_generation);
    let group_repeat_generation_handler = Arc::clone(&group_repeat_generation);

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, event_shortcut, event| {
                    let (generation_state, cycle_event) = if event_shortcut == &handler_shortcut {
                        (&repeat_generation_handler, "switcher:cycle")
                    } else if event_shortcut == &group_handler_shortcut {
                        (&group_repeat_generation_handler, "switcher:group-cycle")
                    } else {
                        return;
                    };

                    match event.state() {
                        ShortcutState::Pressed => {
                            // A generation keeps delayed loops from earlier quick taps from
                            // joining the current hold after they wake up.
                            let generation = generation_state
                                .fetch_add(1, Ordering::AcqRel)
                                .wrapping_add(1);

                            show_switcher_window(app);
                            let _ = app.emit(cycle_event, ());

                            let repeat_generation = Arc::clone(generation_state);
                            let app = app.clone();
                            let cycle_event = cycle_event.to_string();
                            thread::spawn(move || {
                                thread::sleep(Duration::from_millis(280));
                                while repeat_generation.load(Ordering::Acquire) == generation {
                                    let _ = app.emit(&cycle_event, ());
                                    thread::sleep(Duration::from_millis(100));
                                }
                            });
                        }
                        ShortcutState::Released => {
                            generation_state.fetch_add(1, Ordering::AcqRel);
                            let _ = app.emit("switcher:commit", ());
                        }
                    }
                })
                .build(),
        )
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => {
                show_switcher_window(app);
                let _ = app.emit("switcher:open", ());
            }
            "auto_start" => {
                match is_auto_start_enabled().and_then(|enabled| update_auto_start(app, !enabled)) {
                    Ok(()) => refresh_tray_menu(app),
                    Err(error) => {
                        let _ = app.emit("settings:error", error.to_string());
                    }
                }
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
            update_settings,
            apply_switcher_window_bounds
        ])
        .setup(move |app| {
            if let Err(error) = app
                .global_shortcut()
                .register(register_shortcut)
                .and_then(|_| app.global_shortcut().register(group_register_shortcut))
            {
                app.handle()
                    .emit("hotkey:error", error.to_string())
                    .unwrap_or_default();
            }

            let menu = build_tray_menu(app.handle())?;

            let tray = TrayIconBuilder::with_id(TRAY_ID)
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
