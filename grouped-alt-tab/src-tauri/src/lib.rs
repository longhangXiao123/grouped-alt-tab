mod settings;
mod window_manager;

use settings::{
    is_auto_start_enabled, load_settings, save_settings, update_auto_start, SwitcherSettings,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use tauri::{
    menu::{CheckMenuItemBuilder, MenuBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewWindow,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use window_manager::{
    activate_hwnd, close_hwnd, cover_monitor, list_groups, maximize_restore_hwnd,
    minimize_hwnd, move_to_next_monitor_hwnd, recapture_thumbnail, toggle_topmost_hwnd, AppGroup,
};
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

const DEFAULT_HOTKEY: &str = "Alt+`";

/// 一次热键切换会话的共享状态。
/// generation 用来让旧的延时循环失效,watch_active 标记 Alt 松开监视线程是否在跑,
/// bindings 保存当前生效的两个快捷键,热键 handler 用它识别事件来源。
#[derive(Default)]
struct HotkeySession {
    repeat_generation: AtomicU64,
    group_repeat_generation: AtomicU64,
    watch_active: AtomicBool,
    bindings: Mutex<Option<ShortcutBindings>>,
}

struct ShortcutBindings {
    main: Shortcut,
    group: Shortcut,
}

fn session_bindings(
    session: &HotkeySession,
) -> std::sync::MutexGuard<'_, Option<ShortcutBindings>> {
    session
        .bindings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 解析并注册新的主热键(组间切换自动叠加 Shift),成功后换掉旧绑定。
/// 注册失败(格式错误或被占用)时返回错误,旧热键保持不变。
fn apply_hotkeys(app: &AppHandle, spec: &str) -> Result<(), String> {
    let trimmed = spec.trim();
    let main: Shortcut = trimmed
        .parse()
        .map_err(|_| format!("无法识别热键 \"{trimmed}\",示例:Ctrl+Alt+Q"))?;
    if main.mods.is_empty() {
        return Err(format!("热键 \"{trimmed}\" 需要至少一个修饰键(Ctrl/Alt/Shift/Win)"));
    }
    let group = Shortcut::new(Some(main.mods | Modifiers::SHIFT), main.key);

    let global_shortcut = app.global_shortcut();
    let session = app.state::<HotkeySession>();
    let mut bindings = session_bindings(&session);
    let same_main = bindings.as_ref().map(|b| b.main == main).unwrap_or(false);
    let same_group = bindings.as_ref().map(|b| b.group == group).unwrap_or(false);

    // 相同热键重复注册会报错,跳过已生效的组合
    if !same_main {
        global_shortcut
            .register(main)
            .map_err(|_| format!("热键 {trimmed} 注册失败,可能已被其他程序占用"))?;
    }
    if !same_group {
        if let Err(error) = global_shortcut.register(group) {
            if !same_main {
                let _ = global_shortcut.unregister(main);
            }
            return Err(format!("组间切换热键(热键+Shift)注册失败:{error}"));
        }
    }

    if let Some(old) = bindings.replace(ShortcutBindings { main, group }) {
        if !same_main {
            let _ = global_shortcut.unregister(old.main);
        }
        if !same_group {
            let _ = global_shortcut.unregister(old.group);
        }
    }
    Ok(())
}

#[tauri::command]
fn list_window_groups(app: AppHandle) -> AppResult<Vec<AppGroup>> {
    list_groups(&app).map_err(AppError::from)
}

#[tauri::command]
fn activate_window(hwnd: String) -> AppResult<()> {
    activate_hwnd(hwnd).map_err(AppError::from)
}

#[tauri::command]
fn close_window(hwnd: String) -> AppResult<()> {
    close_hwnd(hwnd).map_err(AppError::from)
}

#[tauri::command]
fn minimize_window(hwnd: String) -> AppResult<()> {
    minimize_hwnd(hwnd).map_err(AppError::from)
}

#[tauri::command]
fn toggle_topmost(hwnd: String) -> AppResult<bool> {
    toggle_topmost_hwnd(hwnd).map_err(AppError::from)
}

#[tauri::command]
fn maximize_restore_window(hwnd: String) -> AppResult<()> {
    maximize_restore_hwnd(hwnd).map_err(AppError::from)
}

#[tauri::command]
fn move_window_to_next_monitor(hwnd: String) -> AppResult<()> {
    move_to_next_monitor_hwnd(hwnd).map_err(AppError::from)
}

#[tauri::command]
fn recapture_window_thumbnail(hwnd: String) -> AppResult<Option<String>> {
    Ok(recapture_thumbnail(hwnd))
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
    let previous = load_settings(&app).map_err(AppError::from)?;
    save_settings(&app, &settings).map_err(AppError::from)?;
    if let Err(message) = apply_hotkeys(&app, &settings.hotkey) {
        // 新热键不可用:回滚到之前的设置和热键,让前端展示错误
        let _ = save_settings(&app, &previous);
        let _ = apply_hotkeys(&app, &previous.hotkey);
        return Err(AppError::Message(message));
    }
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

    // 自定义尺寸模式同样定位到光标所在的显示器
    let (monitor_x, monitor_y, monitor_width, monitor_height) =
        match unsafe { window_manager::monitor_under_cursor() } {
            Some(info) => {
                let rect = info.rcMonitor;
                (rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top)
            }
            None => {
                let monitor = window
                    .current_monitor()?
                    .or_else(|| window.primary_monitor().ok().flatten())
                    .ok_or_else(|| anyhow::anyhow!("no monitor available"))?;
                let position = *monitor.position();
                let size = *monitor.size();
                (position.x, position.y, size.width as i32, size.height as i32)
            }
        };

    let size = PhysicalSize::new(
        settings.window_width.clamp(720, 7680),
        settings.window_height.clamp(460, 4320),
    );
    let x_offset = ((monitor_width as u32).saturating_sub(size.width) / 2) as i32;
    let y_offset = ((monitor_height as u32).saturating_sub(size.height) / 2) as i32;
    let position = PhysicalPosition::new(monitor_x + x_offset, monitor_y + y_offset);

    window.set_position(position)?;
    window.set_size(size)?;

    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, event_shortcut, event| {
                    let session = app.state::<HotkeySession>();
                    let (generation_state, cycle_event, is_group) = {
                        let bindings = session_bindings(&session);
                        match bindings.as_ref() {
                            Some(bindings) if bindings.main == *event_shortcut => {
                                (&session.repeat_generation, "switcher:cycle", false)
                            }
                            Some(bindings) if bindings.group == *event_shortcut => {
                                (&session.group_repeat_generation, "switcher:group-cycle", true)
                            }
                            _ => return,
                        }
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
            close_window,
            minimize_window,
            toggle_topmost,
            maximize_restore_window,
            move_window_to_next_monitor,
            recapture_window_thumbnail,
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

            let settings = match load_settings(app.handle()) {
                Ok(settings) => settings,
                Err(error) => {
                    eprintln!("failed to load settings, using defaults: {error}");
                    SwitcherSettings::default()
                }
            };
            if let Err(error) = apply_hotkeys(app.handle(), &settings.hotkey) {
                eprintln!("configured hotkey unavailable ({error}), falling back to {DEFAULT_HOTKEY}");
                let _ = apply_hotkeys(app.handle(), DEFAULT_HOTKEY);
            }

            // 启动时预热缩略图缓存,让首次唤出切换器就能命中缓存(否则要等约 1 秒截图)
            let _ = window_manager::list_groups(app.handle());

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
