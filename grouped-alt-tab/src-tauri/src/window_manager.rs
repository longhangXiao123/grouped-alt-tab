use anyhow::{anyhow, Context};
use serde::Serialize;
use std::{collections::BTreeMap, ffi::c_void};
use tauri::{AppHandle, Manager};
use windows::Win32::{
        Foundation::{BOOL, CloseHandle, HWND, LPARAM, MAX_PATH},
        System::{
            ProcessStatus::K32GetModuleFileNameExW,
            Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ},
        },
        UI::WindowsAndMessaging::{
            EnumWindows, GetAncestor, GetLastActivePopup, GetWindow, GetWindowLongW,
            GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
            IsWindowVisible, SetForegroundWindow, ShowWindow, GWL_EXSTYLE, GA_ROOTOWNER,
            GW_OWNER, SW_RESTORE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
        },
};

#[derive(Debug, Clone, Serialize)]
pub struct WindowInfo {
    pub hwnd: String,
    pub pid: u32,
    pub title: String,
    pub process_name: String,
    pub exe_path: String,
    pub icon: Option<String>,
    pub is_minimized: bool,
    pub last_active_rank: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppGroup {
    pub group_id: String,
    pub app_name: String,
    pub exe_path: String,
    pub windows: Vec<WindowInfo>,
    pub active_window_hwnd: Option<String>,
}

struct EnumContext {
    windows: Vec<WindowInfo>,
    self_pid: u32,
    excluded_processes: Vec<String>,
}

pub fn list_groups(app: &AppHandle) -> anyhow::Result<Vec<AppGroup>> {
    let settings = crate::settings::load_settings(app)?;
    let mut ctx = EnumContext {
        windows: Vec::new(),
        self_pid: std::process::id(),
        excluded_processes: settings
            .excluded_processes
            .iter()
            .map(|item| item.to_ascii_lowercase())
            .collect(),
    };

    unsafe {
        EnumWindows(Some(enum_window_proc), LPARAM((&mut ctx as *mut EnumContext) as isize))
            .ok()
            .context("EnumWindows failed")?;
    }

    let main_hwnd = app
        .get_webview_window("main")
        .and_then(|window| window.hwnd().ok())
        .map(|hwnd| (hwnd.0 as isize).to_string());

    let mut grouped = BTreeMap::<String, AppGroup>::new();
    for window in ctx.windows.into_iter().filter(|window| Some(&window.hwnd) != main_hwnd.as_ref()) {
        let key = window.exe_path.to_ascii_lowercase();
        let group = grouped.entry(key.clone()).or_insert_with(|| AppGroup {
            group_id: key.clone(),
            app_name: display_name(&window),
            exe_path: window.exe_path.clone(),
            windows: Vec::new(),
            active_window_hwnd: None,
        });

        if group.active_window_hwnd.is_none() {
            group.active_window_hwnd = Some(window.hwnd.clone());
        }
        group.windows.push(window);
    }

    let mut groups: Vec<AppGroup> = grouped.into_values().collect();
    for group in &mut groups {
        group.windows.sort_by_key(|window| window.last_active_rank);
    }
    groups.sort_by(|a, b| {
        let a_rank = a.windows.first().map(|window| window.last_active_rank).unwrap_or(usize::MAX);
        let b_rank = b.windows.first().map(|window| window.last_active_rank).unwrap_or(usize::MAX);
        a_rank.cmp(&b_rank).then_with(|| a.app_name.cmp(&b.app_name))
    });

    Ok(groups)
}

pub fn activate_hwnd(hwnd: String) -> anyhow::Result<()> {
    let hwnd_value = hwnd
        .parse::<isize>()
        .with_context(|| format!("invalid window handle: {}", hwnd))?;
    let hwnd = HWND(hwnd_value as *mut c_void);
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }

        if SetForegroundWindow(hwnd).as_bool() {
            Ok(())
        } else {
            Err(anyhow!("Windows did not allow this window to be focused"))
        }
    }
}

unsafe extern "system" fn enum_window_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut EnumContext);

    if let Some(window) = inspect_window(hwnd, ctx) {
        ctx.windows.push(window);
    }

    BOOL(1)
}

unsafe fn inspect_window(hwnd: HWND, ctx: &EnumContext) -> Option<WindowInfo> {
    if !is_switchable_window(hwnd) {
        return None;
    }

    let mut pid = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == 0 || pid == ctx.self_pid {
        return None;
    }

    let title = get_window_title(hwnd)?;
    let exe_path = get_process_path(pid).unwrap_or_default();
    let process_name = process_name_from_path(&exe_path);

    if ctx.excluded_processes.contains(&process_name.to_ascii_lowercase()) {
        return None;
    }

    Some(WindowInfo {
        hwnd: (hwnd.0 as isize).to_string(),
        pid,
        title,
        process_name,
        exe_path,
        icon: None,
        is_minimized: IsIconic(hwnd).as_bool(),
        last_active_rank: ctx.windows.len(),
    })
}

unsafe fn is_switchable_window(hwnd: HWND) -> bool {
    if !IsWindowVisible(hwnd).as_bool() {
        return false;
    }

    let root_owner = GetAncestor(hwnd, GA_ROOTOWNER);
    let last_active_popup = GetLastActivePopup(root_owner);
    if last_active_popup != hwnd && IsWindowVisible(last_active_popup).as_bool() {
        return false;
    }

    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    let is_tool = (ex_style & WS_EX_TOOLWINDOW.0) != 0;
    let is_app = (ex_style & WS_EX_APPWINDOW.0) != 0;
    let has_owner = GetWindow(hwnd, GW_OWNER).map(|owner| !owner.0.is_null()).unwrap_or(false);

    if is_tool {
        return false;
    }

    !has_owner || is_app
}

unsafe fn get_window_title(hwnd: HWND) -> Option<String> {
    let len = GetWindowTextLengthW(hwnd);
    if len <= 0 {
        return None;
    }

    let mut buffer = vec![0u16; (len + 1) as usize];
    let copied = GetWindowTextW(hwnd, &mut buffer);
    if copied <= 0 {
        return None;
    }

    let title = String::from_utf16_lossy(&buffer[..copied as usize]).trim().to_string();
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

unsafe fn get_process_path(pid: u32) -> anyhow::Result<String> {
    let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, false, pid)
        .with_context(|| format!("failed to open process {}", pid))?;
    let mut buffer = vec![0u16; MAX_PATH as usize];
    let len = K32GetModuleFileNameExW(process, None, &mut buffer);
    CloseHandle(process).ok();

    if len == 0 {
        return Err(anyhow!("failed to read process path for {}", pid));
    }

    Ok(String::from_utf16_lossy(&buffer[..len as usize]))
}

fn process_name_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Unknown")
        .to_string()
}

fn display_name(window: &WindowInfo) -> String {
    window
        .process_name
        .strip_suffix(".exe")
        .unwrap_or(&window.process_name)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{display_name, process_name_from_path, WindowInfo};

    fn sample_window(exe_path: &str, process_name: &str) -> WindowInfo {
        WindowInfo {
            hwnd: "1".to_string(),
            pid: 10,
            title: "Test".to_string(),
            process_name: process_name.to_string(),
            exe_path: exe_path.to_string(),
            icon: None,
            is_minimized: false,
            last_active_rank: 0,
        }
    }

    #[test]
    fn process_name_comes_from_path() {
        assert_eq!(process_name_from_path(r"C:\Windows\explorer.exe"), "explorer.exe");
    }

    #[test]
    fn display_name_removes_exe_suffix() {
        let window = sample_window(r"C:\Windows\explorer.exe", "explorer.exe");
        assert_eq!(display_name(&window), "explorer");
    }
}
