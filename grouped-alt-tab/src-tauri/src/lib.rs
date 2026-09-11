mod settings;
mod window_manager;

use settings::{
    is_auto_start_enabled, load_settings, save_settings, update_auto_start, SwitcherSettings,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;
use tauri::{
    menu::{CheckMenuItemBuilder, MenuBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewWindow,
};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use window_manager::{activate_hwnd, cover_monitor, list_groups, AppGroup};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_MENU};

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

/// 一次热键切换会话的共享状态。
/// generation 用来让旧的延时循环失效,watch_active 标记 Alt 松开监视线程是否在跑。
#[derive(Default)]
struct HotkeySession {
    repeat_generation: AtomicU64,
    group_repeat_generation: AtomicU64,
    watch_active: AtomicBool,
}

#[tauri::command]
fn list_window_groups(app: AppHandle) -> AppResult<Vec<AppGroup>> {
    list_groups(&app).map_err(AppError::from)
}

#[tauri::command]
fn activate_window(hwnd: String) -> AppResult<()> {
    activate_hwnd(hwnd).map_err(AppError::from)
}

/// 用户按 Esc 取消切换时调用:停掉自动循环和 Alt 监视,避免松开 Alt 时误提交。
#[tauri::command]
fn cancel_switcher_session(app: AppHandle) {
    let session = app.state::<HotkeySession>();
    session.repeat_generation.fetch_add(1, Ordering::AcqRel);
    session.group_repeat_generation.fetch_add(1, Ordering::AcqRel);
    session.watch_active.store(false, Ordering::Release);
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

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, event_shortcut, event| {
                    let session = app.state::<HotkeySession>();
                    let (generation_state, cycle_event, is_group) =
                        if event_shortcut == &handler_shortcut {
                            (&session.repeat_generation, "switcher:cycle", false)
                        } else if event_shortcut == &group_handler_shortcut {
                            (&session.group_repeat_generation, "switcher:group-cycle", true)
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

                            // `\` 按住期间以 100ms 步进自动循环;`\` 提前松开(Alt 仍按住)时
                            // generation 变化会让循环退出,但切换器保持打开。
                            let repeat_app = app.clone();
                            let cycle_event = cycle_event.to_string();
                            thread::spawn(move || {
                                thread::sleep(Duration::from_millis(280));
                                loop {
                                    let state = if is_group {
                                        repeat_app
                                            .state::<HotkeySession>()
                                            .group_repeat_generation
                                            .load(Ordering::Acquire)
                                    } else {
                                        repeat_app
                                            .state::<HotkeySession>()
                                            .repeat_generation
                                            .load(Ordering::Acquire)
                                    };
                                    if state != generation {
                                        break;
                                    }
                                    let _ = repeat_app.emit(&cycle_event, ());
                                    thread::sleep(Duration::from_millis(100));
                                }
                            });

                            // Alt+Tab 语义:提交发生在 Alt 松开时,而不是 `\` 松开时。
                            // 监视线程每次会话只启动一个;托盘打开的会话不经过这里,
                            // 不会被 Alt 松开误提交。
                            if !session.watch_active.swap(true, Ordering::AcqRel) {
                                let watch_app = app.clone();
                                thread::spawn(move || loop {
                                    thread::sleep(Duration::from_millis(20));
                                    let session = watch_app.state::<HotkeySession>();
                                    if !session.watch_active.load(Ordering::Acquire) {
                                        break;
                                    }
                                    let alt_down =
                                        unsafe { GetAsyncKeyState(VK_MENU.0 as i32) } as u16
                                            & 0x8000
                                            != 0;
                                    if !alt_down {
                                        session.repeat_generation.fetch_add(1, Ordering::AcqRel);
                                        session
                                            .group_repeat_generation
                                            .fetch_add(1, Ordering::AcqRel);
                                        session.watch_active.store(false, Ordering::Release);
                                        let _ = watch_app.emit("switcher:commit", ());
                                        break;
                                    }
                                });
                            }
                        }
                        ShortcutState::Released => {
                            // `\` 松开时只停自动循环。只要 Alt 还按着就保持切换器打开,
                            // 提交交给上面的 Alt 监视线程;若 Alt 已先松开,提交也已发生过,
                            // 这里不做任何事。
                            generation_state.fetch_add(1, Ordering::AcqRel);
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
            apply_switcher_window_bounds,
            cancel_switcher_session
        ])
        .setup(move |app| {
            app.manage(HotkeySession::default());

            // 系统级亚克力材质：模糊窗口背后的真实桌面内容，
            // 前端半透明面板叠加在上面形成液态玻璃分层。
            #[cfg(target_os = "windows")]
            if let Some(window) = app.get_webview_window("main") {
                if let Err(error) = window_vibrancy::apply_acrylic(&window, None) {
                    eprintln!("failed to apply acrylic window material: {error}");
                }
            }

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
