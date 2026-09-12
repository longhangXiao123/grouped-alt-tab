# Grouped Alt-Tab

Windows 上按应用分组的窗口切换器。按住热键浏览所有窗口(按应用自动分组、带实时缩略图与应用图标),松开热键跳转,并提供完整的窗口管理操作。基于 Tauri 2 + React,使用系统 Acrylic 材质与液态玻璃风格界面。

## 功能

- **按应用分组**:同一进程的窗口归为一组,组按最近使用排序,组间、组内独立导航
- **液态玻璃界面**:系统 Acrylic 模糊 + 半透明渐变面板,深浅色跟随系统
- **Alt+Tab 式语义**:按住浏览,松开热键(而非子键)才跳转;快速点按 = 直接切到下一个窗口
- **真实验应用图标**:从 exe 提取并按进程缓存
- **实时缩略图**:窗口尺寸或标题变化才重新截图,其余走缓存;应用启动时预热,弹出即秒开
- **窗口管理**:关闭、关闭整组、最小化、最大化/还原、置顶、移动到下一显示器
- **虚拟桌面感知**:可只显示当前虚拟桌面的窗口(在不支持的 Windows 构建上自动禁用)
- **可配置热键**:任意 Ctrl/Alt/Shift/Win + 键组合,保存时自动检测冲突
- **排除进程**:指定不显示的进程,支持从运行中的窗口挑选
- **多显示器**:切换器出现在光标所在的显示器
- **开机自启、托盘图标、全屏/自定义尺寸两种窗口模式**

## 快捷键

| 按键 | 作用 |
| --- | --- |
| 按住 `Alt + \`` | 打开切换器并循环切换窗口,松开跳转 |
| 按住时点按 `` ` `` | 逐个切换窗口 |
| `Alt + Shift + \`` | 在应用组之间切换 |
| `Enter` | 激活当前选中的窗口 |
| `Esc` | 取消并隐藏 |
| `↑` / `↓` | 组内上/下一个窗口 |
| `←` / `→` 或 `Tab` | 上/下一个应用组 |
| `W` / `Shift + W` | 关闭选中窗口 / 关闭整组 |
| `M` / `X` | 最小化 / 最大化·还原 |
| `T` | 置顶 / 取消置顶 |
| `Ctrl + →` | 移动窗口到下一个显示器 |
| 鼠标滚轮 | 侧栏上滚换分组,详情区上滚换窗口 |
| 右键窗口行 | 打开窗口操作菜单 |

设置面板(右上角齿轮):热键、窗口大小(全屏/自定义)、开机自启、只显示当前桌面、排除的进程。

## 配置

配置保存在 `%APPDATA%\dev.local.grouped-alt-tab\settings.json`,包含热键、窗口模式、排除进程等字段,推荐通过设置面板修改。

## 环境依赖

- Node.js 与 pnpm
- Rust/Cargo(建议通过 rustup 安装)
- Microsoft Edge WebView2 Runtime
- Visual Studio Build Tools(MSVC 与 Windows SDK 组件)

## 开发与构建

```bash
pnpm install
pnpm tauri dev    # 开发调试
pnpm tauri build  # 产出 exe 与安装包(NSIS / MSI)
```

后端测试:

```bash
cd src-tauri && cargo test
```

产物位于 `src-tauri/target/release/grouped-alt-tab.exe` 与 `src-tauri/target/release/bundle/`。

> 注意:直接 `cargo build --release` 不会启用 `custom-protocol` feature,产出的二进制会尝试加载开发服务器地址而非内嵌前端,请始终通过 `pnpm tauri build` 打包。

## 平台说明

- 需要 Windows 10 及以上;Acrylic 材质与圆角在 Windows 11 上效果最佳,Windows 10 使用兼容路径
- 虚拟桌面过滤使用官方 `IVirtualDesktopManager`;个别 Insider 构建未注册该 COM 类时会自动禁用该过滤并在日志中提示
