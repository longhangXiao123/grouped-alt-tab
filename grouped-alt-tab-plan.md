# Grouped Alt-Tab Plan

## Summary

Build a Windows 11 focused desktop window switcher that groups open windows by application. The code lives in `grouped-alt-tab/` and uses Tauri v2 with a Rust backend and a React/TypeScript frontend.

The v1 target is a usable grouped switcher opened with `Alt + \``. It shows application groups and their windows using icons/titles, then activates the selected window. Real DWM thumbnails are reserved for v2.

## Environment

- Available on this machine at project start: Node.js `v25.2.1`, npm `11.6.2`, pnpm `10.30.2`, vfox `0.9.2`, WebView2 `149.0.4022.80`.
- Missing at project start: `rustc`, `cargo`, `rustup`, Visual Studio/MSVC Build Tools with Windows SDK components.
- Rust will be installed by the user, not by vfox or this project.
- Tauri CLI is a project dependency and should be run through `pnpm tauri ...`.
- Install Rust with rustup and install Visual Studio Build Tools/MSVC before running `pnpm tauri dev`.

## Implementation

- Create `grouped-alt-tab/` as a Tauri v2 + Vite + React + TypeScript app.
- Register a global `Alt + \`` shortcut that emits `switcher:open` to the frontend.
- Enumerate visible top-level windows with Win32 APIs and filter out hidden/tool/no-title/self windows.
- Group windows by normalized executable path, using the process name as the display name.
- Activate windows through Win32 by restoring minimized windows and calling `SetForegroundWindow`.
- Keep settings minimal for v1: hotkey, grouping mode, preview mode, excluded processes.

## Interfaces

- `list_window_groups() -> Vec<AppGroup>`
- `activate_window(hwnd: u64) -> Result<()>`
- `get_settings() -> SwitcherSettings`
- `update_settings(settings: SwitcherSettings) -> Result<()>`

## Test Plan

- Frontend: `pnpm lint` and `pnpm build`.
- Rust after user installs Rust: `cargo test` inside `src-tauri/`.
- Full app after user installs Rust: `pnpm tauri dev`.
- Manual Windows 11 check: open multiple Explorer, Edge/Chrome, Notepad windows; press `Alt + \``; verify windows are grouped by app and activation works.

## Assumptions

- Do not replace system `Alt + Tab` in v1.
- v1 uses icons and titles, not live thumbnails.
- The app runs as a normal user. Elevated windows may be limited by Windows foreground activation rules.
