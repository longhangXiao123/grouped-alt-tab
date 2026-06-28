use anyhow::{anyhow, Context};
use base64::{engine::general_purpose, Engine as _};
use image::{codecs::png::PngEncoder, ColorType, ImageEncoder};
use serde::Serialize;
use std::{collections::BTreeMap, ffi::c_void};
use tauri::{AppHandle, Manager, WebviewWindow};
use windows::Win32::{
    Foundation::{CloseHandle, BOOL, HWND, LPARAM, MAX_PATH, RECT},
    Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits,
        GetMonitorInfoW, GetWindowDC, MonitorFromWindow, ReleaseDC, SelectObject, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HGDIOBJ, MONITORINFO,
        MONITOR_DEFAULTTONEAREST, SRCCOPY,
    },
    Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS},
    System::{
        ProcessStatus::K32GetModuleFileNameExW,
        Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ},
    },
    UI::WindowsAndMessaging::{
        EnumWindows, GetAncestor, GetLastActivePopup, GetWindow, GetWindowLongW, GetWindowRect,
        GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
        SetForegroundWindow, SetWindowPos, ShowWindow, GA_ROOTOWNER, GWL_EXSTYLE, GW_OWNER,
        HWND_TOPMOST, PW_RENDERFULLCONTENT, SWP_SHOWWINDOW, SW_RESTORE, WS_EX_APPWINDOW,
        WS_EX_TOOLWINDOW,
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
    pub thumbnail: Option<String>,
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
        EnumWindows(
            Some(enum_window_proc),
            LPARAM((&mut ctx as *mut EnumContext) as isize),
        )
        .ok()
        .context("EnumWindows failed")?;
    }

    let main_hwnd = app
        .get_webview_window("main")
        .and_then(|window| window.hwnd().ok())
        .map(|hwnd| (hwnd.0 as isize).to_string());

    let mut grouped = BTreeMap::<String, AppGroup>::new();
    for window in ctx
        .windows
        .into_iter()
        .filter(|window| Some(&window.hwnd) != main_hwnd.as_ref())
    {
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
        let a_rank = a
            .windows
            .first()
            .map(|window| window.last_active_rank)
            .unwrap_or(usize::MAX);
        let b_rank = b
            .windows
            .first()
            .map(|window| window.last_active_rank)
            .unwrap_or(usize::MAX);
        a_rank
            .cmp(&b_rank)
            .then_with(|| a.app_name.cmp(&b.app_name))
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

pub fn cover_monitor(window: &WebviewWindow) -> anyhow::Result<()> {
    let hwnd = window.hwnd()?;
    let hwnd = HWND(hwnd.0 as *mut c_void);

    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };

        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return Err(anyhow!("failed to read monitor bounds"));
        }

        let rect = info.rcMonitor;
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_SHOWWINDOW,
        )
        .context("failed to cover monitor")?;
    }

    Ok(())
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

    if ctx
        .excluded_processes
        .contains(&process_name.to_ascii_lowercase())
    {
        return None;
    }

    Some(WindowInfo {
        hwnd: (hwnd.0 as isize).to_string(),
        pid,
        title,
        process_name,
        exe_path,
        icon: None,
        thumbnail: capture_window_thumbnail(hwnd).ok(),
        is_minimized: IsIconic(hwnd).as_bool(),
        last_active_rank: ctx.windows.len(),
    })
}

unsafe fn capture_window_thumbnail(hwnd: HWND) -> anyhow::Result<String> {
    let mut rect = RECT::default();
    GetWindowRect(hwnd, &mut rect).context("failed to read window rect")?;

    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width <= 0 || height <= 0 || width > 10_000 || height > 10_000 {
        return Err(anyhow!("window size is not capturable"));
    }

    let window_dc = GetWindowDC(hwnd);
    if window_dc.0.is_null() {
        return Err(anyhow!("failed to get window device context"));
    }

    let mem_dc = CreateCompatibleDC(window_dc);
    if mem_dc.0.is_null() {
        ReleaseDC(hwnd, window_dc);
        return Err(anyhow!("failed to create compatible device context"));
    }

    let bitmap = CreateCompatibleBitmap(window_dc, width, height);
    if bitmap.0.is_null() {
        let _ = DeleteDC(mem_dc);
        ReleaseDC(hwnd, window_dc);
        return Err(anyhow!("failed to create compatible bitmap"));
    }

    let old_object = SelectObject(mem_dc, HGDIOBJ(bitmap.0));
    let printed = PrintWindow(hwnd, mem_dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool();
    if !printed {
        let _ = BitBlt(mem_dc, 0, 0, width, height, window_dc, 0, 0, SRCCOPY);
    }

    let result = bitmap_to_png_data_url(window_dc, bitmap, width, height);

    SelectObject(mem_dc, old_object);
    let _ = DeleteObject(HGDIOBJ(bitmap.0));
    let _ = DeleteDC(mem_dc);
    ReleaseDC(hwnd, window_dc);

    result
}

unsafe fn bitmap_to_png_data_url(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    bitmap: HBITMAP,
    width: i32,
    height: i32,
) -> anyhow::Result<String> {
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bgra = vec![0u8; (width * height * 4) as usize];
    let lines = GetDIBits(
        hdc,
        bitmap,
        0,
        height as u32,
        Some(bgra.as_mut_ptr() as *mut c_void),
        &mut info,
        DIB_RGB_COLORS,
    );
    if lines == 0 {
        return Err(anyhow!("failed to read bitmap pixels"));
    }

    let mut rgba = Vec::with_capacity(bgra.len());
    for pixel in bgra.chunks_exact(4) {
        rgba.push(pixel[2]);
        rgba.push(pixel[1]);
        rgba.push(pixel[0]);
        rgba.push(255);
    }

    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(&rgba, width as u32, height as u32, ColorType::Rgba8.into())
        .context("failed to encode window thumbnail")?;

    Ok(format!(
        "data:image/png;base64,{}",
        general_purpose::STANDARD.encode(png)
    ))
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
    let has_owner = GetWindow(hwnd, GW_OWNER)
        .map(|owner| !owner.0.is_null())
        .unwrap_or(false);

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

    let title = String::from_utf16_lossy(&buffer[..copied as usize])
        .trim()
        .to_string();
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

unsafe fn get_process_path(pid: u32) -> anyhow::Result<String> {
    let process = OpenProcess(
        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
        false,
        pid,
    )
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
            thumbnail: None,
            is_minimized: false,
            last_active_rank: 0,
        }
    }

    #[test]
    fn process_name_comes_from_path() {
        assert_eq!(
            process_name_from_path(r"C:\Windows\explorer.exe"),
            "explorer.exe"
        );
    }

    #[test]
    fn display_name_removes_exe_suffix() {
        let window = sample_window(r"C:\Windows\explorer.exe", "explorer.exe");
        assert_eq!(display_name(&window), "explorer");
    }
}
