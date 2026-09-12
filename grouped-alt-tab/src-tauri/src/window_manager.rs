use anyhow::{anyhow, Context};
use base64::{engine::general_purpose, Engine as _};
use image::{codecs::png::PngEncoder, ColorType, ImageEncoder};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    ffi::c_void,
    sync::{Mutex, OnceLock},
};
use tauri::{AppHandle, Manager, WebviewWindow};
use windows::core::{GUID, PCWSTR};
use windows::Win32::{
    Foundation::{CloseHandle, BOOL, HWND, LPARAM, MAX_PATH, POINT, RECT, WPARAM},
    Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, EnumDisplayMonitors,
        GetDC, GetDIBits, GetMonitorInfoW, GetObjectW, GetWindowDC, MonitorFromPoint,
        MonitorFromWindow, ReleaseDC, SelectObject, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST,
        SRCCOPY,
    },
    Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
    Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS},
    System::{
        Com::{CoCreateInstance, CoIncrementMTAUsage, CLSCTX_ALL},
        ProcessStatus::K32GetModuleFileNameExW,
        Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ},
    },
    UI::Shell::{
        IVirtualDesktopManager, SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON,
    },
    UI::WindowsAndMessaging::{
        DestroyIcon, EnumWindows, GetAncestor, GetCursorPos, GetIconInfo, GetLastActivePopup,
        GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
        GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed, PostMessageW,
        SetForegroundWindow, SetWindowPos, ShowWindow, GA_ROOTOWNER, GWL_EXSTYLE, GW_OWNER,
        HWND_NOTOPMOST, HWND_TOPMOST, ICONINFO, PW_RENDERFULLCONTENT, SW_MAXIMIZE, SW_MINIMIZE,
        SW_RESTORE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, WM_CLOSE,
        WS_EX_APPWINDOW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, HICON,
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
    pub is_on_current_desktop: bool,
    pub last_active_rank: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppGroup {
    pub group_id: String,
    pub app_name: String,
    pub exe_path: String,
    pub icon: Option<String>,
    pub windows: Vec<WindowInfo>,
    pub active_window_hwnd: Option<String>,
}

struct EnumContext {
    windows: Vec<WindowInfo>,
    self_pid: u32,
    excluded_processes: Vec<String>,
    show_current_desktop_only: bool,
    desktop_manager: Option<IVirtualDesktopManager>,
}

// IVirtualDesktopManager 的文档 CLSID。部分新构建(26200+)的类和接口有变动:
// 旧 CLSID 未注册、新 CLSID 的接口布局不兼容(调用会 AV),因此只走文档 CLSID,
// 不可用时优雅降级为"不过滤"。
const CLSID_VIRTUAL_DESKTOP_MANAGER: GUID =
    GUID::from_u128(0xaa509086_5ca9_4c25_8f95_589d3c07b0f8);

/// 进程内递增一次 MTA 引用,之后任意线程都能使用 COM 服务器对象。
/// cookie 存成 usize 规避 Send/Sync,MTA 随进程存活,不需要归还。
fn mta_cookie() -> Option<usize> {
    static COOKIE: OnceLock<Option<usize>> = OnceLock::new();
    *COOKIE.get_or_init(|| unsafe { CoIncrementMTAUsage().ok().map(|cookie| cookie.0 as usize) })
}

pub unsafe fn virtual_desktop_manager() -> Option<IVirtualDesktopManager> {
    mta_cookie()?;
    match CoCreateInstance(&CLSID_VIRTUAL_DESKTOP_MANAGER, None, CLSCTX_ALL) {
        Ok(manager) => Some(manager),
        Err(_) => {
            static LOGGED: OnceLock<()> = OnceLock::new();
            if LOGGED.set(()).is_ok() {
                eprintln!("virtual desktop manager COM class unavailable; desktop filter disabled");
            }
            None
        }
    }
}

/// 窗口是否在当前虚拟桌面;接口不可用或查询失败一律按"在"处理,不影响原行为。
pub unsafe fn is_on_current_desktop(
    manager: Option<&IVirtualDesktopManager>,
    hwnd: HWND,
) -> bool {
    let Some(manager) = manager else {
        return true;
    };
    manager
        .IsWindowOnCurrentVirtualDesktop(hwnd)
        .map(|on| on.as_bool())
        .unwrap_or(true)
}

/// 缩略图脏缓存:窗口尺寸和标题都没变时复用上次截图,
/// 只有确定变了才重新 PrintWindow。key 是 hwnd 字符串(与序列化一致)。
fn thumbnail_cache() -> &'static Mutex<HashMap<String, CachedThumbnail>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedThumbnail>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

struct CachedThumbnail {
    width: i32,
    height: i32,
    title: String,
    thumbnail: Option<String>,
}

unsafe fn cached_thumbnail(hwnd: HWND, title: &str) -> Option<String> {
    let mut rect = RECT::default();
    if GetWindowRect(hwnd, &mut rect).is_err() {
        return None;
    }
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let key = (hwnd.0 as isize).to_string();

    let mut cache = thumbnail_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(cached) = cache.get(&key) {
        if cached.width == width && cached.height == height && cached.title == title {
            return cached.thumbnail.clone();
        }
    }

    let thumbnail = capture_window_thumbnail(hwnd).ok();
    cache.insert(
        key,
        CachedThumbnail {
            width,
            height,
            title: title.to_string(),
            thumbnail: thumbnail.clone(),
        },
    );
    thumbnail
}

/// 刷新结束后清掉已关闭窗口的缓存条目,hwnd 复用由脏检查兜底。
fn prune_thumbnail_cache(alive_hwnds: &BTreeMap<String, ()>) {
    let mut cache = thumbnail_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.retain(|hwnd, _| alive_hwnds.contains_key(hwnd));
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
        show_current_desktop_only: settings.show_current_desktop_only,
        desktop_manager: unsafe { virtual_desktop_manager() },
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

    let alive_hwnds: BTreeMap<String, ()> = ctx
        .windows
        .iter()
        .map(|window| (window.hwnd.clone(), ()))
        .collect();

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
            icon: icon_for_exe(&window.exe_path),
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

    prune_thumbnail_cache(&alive_hwnds);

    Ok(groups)
}

pub fn activate_hwnd(hwnd: String) -> anyhow::Result<()> {
    let hwnd = parse_hwnd(&hwnd)?;
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

/// 请求关闭窗口:发 WM_CLOSE,交给目标应用自己走保存确认流程。
pub fn close_hwnd(hwnd: String) -> anyhow::Result<()> {
    let hwnd = parse_hwnd(&hwnd)?;
    unsafe {
        PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0))
            .ok()
            .context("failed to post WM_CLOSE")?;
    }
    Ok(())
}

pub fn minimize_hwnd(hwnd: String) -> anyhow::Result<()> {
    let hwnd = parse_hwnd(&hwnd)?;
    unsafe {
        // 窗口本来就最小化时返回 FALSE,不算错误
        let _ = ShowWindow(hwnd, SW_MINIMIZE);
    }
    Ok(())
}

/// 切换窗口置顶,返回切换后的状态(true = 已置顶)。
pub fn toggle_topmost_hwnd(hwnd: String) -> anyhow::Result<bool> {
    let hwnd = parse_hwnd(&hwnd)?;
    unsafe {
        let style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let is_topmost = (style & WS_EX_TOPMOST.0) != 0;
        let insert_after = if is_topmost {
            HWND_NOTOPMOST
        } else {
            HWND_TOPMOST
        };
        SetWindowPos(
            hwnd,
            insert_after,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
        .context("failed to toggle topmost")?;
        Ok(!is_topmost)
    }
}

pub fn maximize_restore_hwnd(hwnd: String) -> anyhow::Result<()> {
    let hwnd = parse_hwnd(&hwnd)?;
    unsafe {
        if IsZoomed(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        } else {
            let _ = ShowWindow(hwnd, SW_MAXIMIZE);
        }
    }
    Ok(())
}

unsafe extern "system" fn enum_monitor_cb(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let monitors = &mut *(lparam.0 as *mut Vec<MONITORINFO>);
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(hmonitor, &mut info).as_bool() {
        monitors.push(info);
    }
    BOOL(1)
}

/// 把窗口移动到下一个显示器(单屏时为空操作)。
pub fn move_to_next_monitor_hwnd(hwnd: String) -> anyhow::Result<()> {
    let hwnd = parse_hwnd(&hwnd)?;
    unsafe {
        let mut monitors: Vec<MONITORINFO> = Vec::new();
        let enumerated = EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(enum_monitor_cb),
            LPARAM(&mut monitors as *mut Vec<MONITORINFO> as isize),
        );
        if !enumerated.as_bool() {
            return Err(anyhow!("EnumDisplayMonitors failed"));
        }

        if monitors.len() < 2 {
            return Ok(());
        }

        let current = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut current_info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(current, &mut current_info);
        let index = monitors
            .iter()
            .position(|info| info.rcMonitor == current_info.rcMonitor)
            .unwrap_or(0);
        let target = &monitors[(index + 1) % monitors.len()].rcMonitor;

        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).context("failed to read window rect")?;
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;

        // 最大化窗口会被系统钉在原屏,先还原再移动
        if IsZoomed(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }

        let x = target.left + ((target.right - target.left) - width) / 2;
        let y = target.top + ((target.bottom - target.top) - height) / 2;
        SetWindowPos(hwnd, HWND_TOPMOST, x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE)
            .context("failed to move window to next monitor")?;
    }
    Ok(())
}

/// 对指定窗口立即重新截图(预览区伪实时),并同步更新缓存。
pub fn recapture_thumbnail(hwnd: String) -> Option<String> {
    let hwnd = parse_hwnd(&hwnd).ok()?;
    let mut rect = RECT::default();
    let title;
    unsafe {
        GetWindowRect(hwnd, &mut rect).ok()?;
        title = get_window_title(hwnd).unwrap_or_default();
    }
    let thumbnail = unsafe { capture_window_thumbnail(hwnd) }.ok();

    let mut cache = thumbnail_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.insert(
        (hwnd.0 as isize).to_string(),
        CachedThumbnail {
            width: rect.right - rect.left,
            height: rect.bottom - rect.top,
            title,
            thumbnail: thumbnail.clone(),
        },
    );
    thumbnail
}

fn parse_hwnd(value: &str) -> anyhow::Result<HWND> {
    let parsed = value
        .parse::<isize>()
        .with_context(|| format!("invalid window handle: {value}"))?;
    Ok(HWND(parsed as *mut c_void))
}

/// 光标所在的显示器——按下热键时用户的注意力在哪块屏,切换器就该出现在哪块屏。
/// 取不到时回退到窗口自身最近的显示器。
pub unsafe fn monitor_under_cursor() -> Option<MONITORINFO> {
    let mut point = POINT::default();
    GetCursorPos(&mut point).ok()?;
    let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(monitor, &mut info).as_bool() {
        Some(info)
    } else {
        None
    }
}

pub fn cover_monitor(window: &WebviewWindow) -> anyhow::Result<()> {
    let hwnd = window.hwnd()?;
    let hwnd = HWND(hwnd.0 as *mut c_void);

    unsafe {
        let info = match monitor_under_cursor() {
            Some(info) => info,
            None => {
                let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                let mut fallback = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if !GetMonitorInfoW(monitor, &mut fallback).as_bool() {
                    return Err(anyhow!("failed to read monitor bounds"));
                }
                fallback
            }
        };

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

    // 虚拟桌面:开启过滤时跳过其他桌面的窗口;关掉时保留并打上角标
    let on_current_desktop = is_on_current_desktop(ctx.desktop_manager.as_ref(), hwnd);
    if ctx.show_current_desktop_only && !on_current_desktop {
        return None;
    }

    let thumbnail = cached_thumbnail(hwnd, &title);
    Some(WindowInfo {
        hwnd: (hwnd.0 as isize).to_string(),
        pid,
        title,
        process_name,
        exe_path,
        icon: None,
        thumbnail,
        is_minimized: IsIconic(hwnd).as_bool(),
        is_on_current_desktop: on_current_desktop,
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

fn icon_cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 取应用图标并转成 PNG data URL,按 exe 路径缓存(含失败结果,避免反复尝试)。
pub fn icon_for_exe(exe_path: &str) -> Option<String> {
    let key = exe_path.to_ascii_lowercase();
    let mut cache = icon_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(cached) = cache.get(&key) {
        return cached.clone();
    }

    let icon = unsafe { extract_icon_data_url(exe_path) };
    cache.insert(key, icon.clone());
    icon
}

unsafe fn extract_icon_data_url(exe_path: &str) -> Option<String> {
    let wide: Vec<u16> = exe_path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut info = SHFILEINFOW::default();
    let ok = SHGetFileInfoW(
        PCWSTR(wide.as_ptr()),
        FILE_ATTRIBUTE_NORMAL,
        Some(&mut info),
        std::mem::size_of::<SHFILEINFOW>() as u32,
        SHGFI_ICON | SHGFI_LARGEICON,
    );
    if ok == 0 || info.hIcon.is_invalid() {
        return None;
    }

    let result = icon_to_png_data_url(info.hIcon);
    let _ = DestroyIcon(info.hIcon);
    result.ok()
}

/// HICON → 32 位 BGRA → 反预乘 alpha → PNG data URL。
unsafe fn icon_to_png_data_url(hicon: HICON) -> anyhow::Result<String> {
    let mut info = ICONINFO::default();
    GetIconInfo(hicon, &mut info).context("GetIconInfo failed")?;
    let color = info.hbmColor;
    if color.is_invalid() {
        // 单色掩膜图标:现代应用极少见,交给前端首字母兜底
        let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        return Err(anyhow!("icon has no color bitmap"));
    }

    let mut bm = BITMAP::default();
    if GetObjectW(
        color,
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut bm as *mut BITMAP as *mut c_void),
    ) == 0
    {
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        return Err(anyhow!("failed to read icon bitmap header"));
    }
    let width = bm.bmWidth;
    let height = bm.bmHeight.abs();
    if width <= 0 || height == 0 {
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        return Err(anyhow!("icon bitmap is empty"));
    }

    let screen_dc = GetDC(None);
    let mem_dc = CreateCompatibleDC(screen_dc);
    let old = SelectObject(mem_dc, HGDIOBJ(color.0));

    let mut bmi = BITMAPINFO {
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
        mem_dc,
        color,
        0,
        height as u32,
        Some(bgra.as_mut_ptr() as *mut c_void),
        &mut bmi,
        DIB_RGB_COLORS,
    );

    SelectObject(mem_dc, old);
    let _ = DeleteDC(mem_dc);
    let _ = ReleaseDC(None, screen_dc);
    let _ = DeleteObject(HGDIOBJ(color.0));
    let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
    if lines == 0 {
        return Err(anyhow!("failed to read icon pixels"));
    }

    // 图标位图是预乘 alpha 的 BGRA,还原成直通 alpha 的 RGBA
    let mut rgba = Vec::with_capacity(bgra.len());
    for pixel in bgra.chunks_exact(4) {
        let (b, g, r, a) = (pixel[0], pixel[1], pixel[2], pixel[3]);
        let unpremultiply = |channel: u8| -> u8 {
            if a == 0 {
                0
            } else if a == 255 {
                channel
            } else {
                ((channel as u32 * 255 + a as u32 / 2) / a as u32) as u8
            }
        };
        rgba.push(unpremultiply(r));
        rgba.push(unpremultiply(g));
        rgba.push(unpremultiply(b));
        rgba.push(a);
    }

    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(&rgba, width as u32, height as u32, ColorType::Rgba8.into())
        .context("failed to encode app icon")?;

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
            is_on_current_desktop: true,
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
