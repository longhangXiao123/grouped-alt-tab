# Grouped Alt-Tab

A Windows 11 focused Tauri app switcher that groups top-level windows by executable path. Press `Alt + \`` to open the switcher.

## Prerequisites

- Node.js and pnpm
- Rust/Cargo installed by the user, preferably through rustup
- Microsoft Edge WebView2 Runtime
- Visual Studio Build Tools with MSVC and Windows SDK components

`pnpm tauri info` currently confirms WebView2 is installed, and reports Rust/Cargo/rustup plus MSVC Build Tools as missing.

## Development

```powershell
pnpm install
pnpm build
pnpm tauri dev
```

After Rust is installed, run backend checks with:

```powershell
Set-Location src-tauri
cargo test
```

## Current Scope

- v1 groups windows by normalized executable path.
- v1 uses app/window labels instead of live thumbnails.
- v1 does not replace the system `Alt + Tab`; it uses `Alt + \``.
