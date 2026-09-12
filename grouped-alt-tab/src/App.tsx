import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { Monitor, RefreshCw, Save, Search, Settings } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { AppGroup, SwitcherSettings } from "./types";

const emptyGroups: AppGroup[] = [];

const defaultSettings: SwitcherSettings = {
  hotkey: "Alt+`",
  group_by: "exe_path",
  preview_mode: "icons",
  window_mode: "fullscreen",
  window_width: 1280,
  window_height: 720,
  auto_start: false,
  show_current_desktop_only: true,
  excluded_processes: ["grouped-alt-tab.exe", "ApplicationFrameHost.exe"]
};

type Selection = {
  group: number;
  window: number;
};

function initials(name: string) {
  return name
    .split(/[\s._-]+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((part) => part[0]?.toUpperCase())
    .join("") || "APP";
}

// event.code → 热键字符串里的键名,与 Rust 端 global-hotkey 的解析规则对应
const CODE_KEYS: Record<string, string> = {
  Backquote: "`",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Semicolon: ";",
  Quote: "'",
  Backslash: "\\",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Space: "Space",
  Tab: "Tab",
  ArrowUp: "ArrowUp",
  ArrowDown: "ArrowDown",
  ArrowLeft: "ArrowLeft",
  ArrowRight: "ArrowRight"
};

function codeToKey(code: string): string | null {
  if (CODE_KEYS[code]) return CODE_KEYS[code];
  const letter = /^Key([A-Z])$/.exec(code)?.[1];
  if (letter) return letter;
  const digit = /^Digit([0-9])$/.exec(code)?.[1];
  if (digit) return digit;
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  return null;
}

export default function App() {
  const [groups, setGroups] = useState<AppGroup[]>(emptyGroups);
  const [selectedGroup, setSelectedGroup] = useState(0);
  const [selectedWindow, setSelectedWindow] = useState(0);
  const [query, setQuery] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settings, setSettings] = useState<SwitcherSettings>(defaultSettings);
  const [savingSettings, setSavingSettings] = useState(false);
  const [hotkeyCapturing, setHotkeyCapturing] = useState(false);
  const [hotkeyHint, setHotkeyHint] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [rowMenu, setRowMenu] = useState<{ x: number; y: number; hwnd: string } | null>(null);
  const groupsRef = useRef<AppGroup[]>(emptyGroups);
  const selectedRef = useRef<Selection>({ group: 0, window: 0 });
  const sessionActiveRef = useRef(false);
  const lastCycleAtRef = useRef(0);
  const shellRef = useRef<HTMLElement | null>(null);
  const hotkeyCapturingRef = useRef(false);
  const noticeTimerRef = useRef(0);

  const showNotice = useCallback((text: string) => {
    setNotice(text);
    window.clearTimeout(noticeTimerRef.current);
    noticeTimerRef.current = window.setTimeout(() => setNotice(null), 1800);
  }, []);
  const searchRef = useRef<HTMLInputElement | null>(null);
  const sidebarRef = useRef<HTMLElement | null>(null);
  const detailRef = useRef<HTMLElement | null>(null);

  // 弹出时把焦点从搜索框挪走,让 W/M 管理键随时可用;想过滤时点击搜索框即可
  const blurSearch = useCallback(() => {
    if (document.activeElement === searchRef.current) {
      searchRef.current?.blur();
    }
  }, []);

  const clampSelection = useCallback((nextGroups: AppGroup[], selection: Selection) => {
    if (nextGroups.length === 0) {
      return { group: 0, window: 0 };
    }

    const group = Math.min(Math.max(selection.group, 0), nextGroups.length - 1);
    const windows = nextGroups[group]?.windows.length ?? 0;
    const window = windows === 0 ? 0 : Math.min(Math.max(selection.window, 0), windows - 1);
    return { group, window };
  }, []);

  const nextSelection = useCallback((nextGroups: AppGroup[], selection: Selection) => {
    if (nextGroups.length === 0) {
      return { group: 0, window: 0 };
    }

    const current = clampSelection(nextGroups, selection);
    const currentGroup = nextGroups[current.group];
    if (!currentGroup) {
      return { group: 0, window: 0 };
    }

    if (current.window + 1 < currentGroup.windows.length) {
      return { group: current.group, window: current.window + 1 };
    }

    const group = (current.group + 1) % nextGroups.length;
    return { group, window: 0 };
  }, [clampSelection]);

  const nextGroupSelection = useCallback((nextGroups: AppGroup[], selection: Selection) => {
    if (nextGroups.length === 0) {
      return { group: 0, window: 0 };
    }

    const current = clampSelection(nextGroups, selection);
    return {
      group: (current.group + 1) % nextGroups.length,
      window: 0
    };
  }, [clampSelection]);

  const prevSelection = useCallback((nextGroups: AppGroup[], selection: Selection) => {
    if (nextGroups.length === 0) {
      return { group: 0, window: 0 };
    }

    const current = clampSelection(nextGroups, selection);
    if (current.window > 0) {
      return { group: current.group, window: current.window - 1 };
    }

    const group = (current.group - 1 + nextGroups.length) % nextGroups.length;
    const lastWindow = Math.max(nextGroups[group].windows.length - 1, 0);
    return { group, window: lastWindow };
  }, [clampSelection]);

  const prevGroupSelection = useCallback((nextGroups: AppGroup[], selection: Selection) => {
    if (nextGroups.length === 0) {
      return { group: 0, window: 0 };
    }

    const current = clampSelection(nextGroups, selection);
    const group = (current.group - 1 + nextGroups.length) % nextGroups.length;
    return { group, window: 0 };
  }, [clampSelection]);

  const filteredGroups = useMemo(() => {
    const term = query.trim().toLowerCase();
    if (!term) return groups;

    return groups
      .map((group) => ({
        ...group,
        windows: group.windows.filter(
          (window) =>
            window.title.toLowerCase().includes(term) ||
            group.app_name.toLowerCase().includes(term) ||
            window.process_name.toLowerCase().includes(term)
        )
      }))
      .filter((group) => group.windows.length > 0);
  }, [groups, query]);

  const activeGroup = filteredGroups[selectedGroup] ?? null;
  const activeWindow = activeGroup?.windows[selectedWindow] ?? null;

  const applyGroups = useCallback((nextGroups: AppGroup[]) => {
    groupsRef.current = nextGroups;
    setGroups(nextGroups);
  }, []);

  // 每次切换器弹出时播放一次液态玻璃入场动画
  const playEntrance = useCallback(() => {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      return;
    }
    shellRef.current?.animate(
      [
        { transform: "scale(0.96) translateY(8px)", opacity: 0.55 },
        { transform: "scale(1) translateY(0)", opacity: 1 }
      ],
      { duration: 260, easing: "cubic-bezier(0.2, 0.9, 0.3, 1.1)" }
    );
  }, []);

  const applySelection = useCallback((selection: Selection) => {
    selectedRef.current = selection;
    setSelectedGroup(selection.group);
    setSelectedWindow(selection.window);
  }, []);

  const refresh = useCallback(async (selection?: Selection) => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<AppGroup[]>("list_window_groups");
      applyGroups(result);
      applySelection(clampSelection(result, selection ?? selectedRef.current));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, [applyGroups, applySelection, clampSelection]);

  const activateWindowByHwnd = useCallback(async (hwnd: string, hide = true) => {
    try {
      await invoke("activate_window", { hwnd });
      sessionActiveRef.current = false;
      if (hide) {
        await getCurrentWindow().hide();
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  const activateSelected = useCallback(async () => {
    if (!activeWindow) return;
    await activateWindowByHwnd(activeWindow.hwnd);
  }, [activateWindowByHwnd, activeWindow]);

  // 窗口管理动作统一按 hwnd 操作,键盘快捷键和右键菜单共用
  const closeWindowByHwnd = useCallback(
    async (hwnd: string) => {
      try {
        await invoke("close_window", { hwnd });
        // 给目标应用一点时间处理关闭,再刷新列表
        await new Promise((resolve) => setTimeout(resolve, 300));
        await refresh();
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      }
    },
    [refresh]
  );

  const minimizeWindowByHwnd = useCallback(
    async (hwnd: string) => {
      try {
        await invoke("minimize_window", { hwnd });
        await refresh();
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      }
    },
    [refresh]
  );

  const maximizeRestoreByHwnd = useCallback(
    async (hwnd: string) => {
      try {
        await invoke("maximize_restore_window", { hwnd });
        await refresh();
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      }
    },
    [refresh]
  );

  const toggleTopmostByHwnd = useCallback(
    async (hwnd: string) => {
      try {
        const topmost = await invoke<boolean>("toggle_topmost", { hwnd });
        showNotice(topmost ? "窗口已置顶" : "已取消置顶");
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      }
    },
    [showNotice]
  );

  const moveToNextMonitorByHwnd = useCallback(
    async (hwnd: string) => {
      try {
        await invoke("move_window_to_next_monitor", { hwnd });
        showNotice("已移动到下一个显示器");
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      }
    },
    [showNotice]
  );

  const closeSelected = useCallback(() => {
    const hwnd = activeWindow?.hwnd;
    if (hwnd) void closeWindowByHwnd(hwnd);
  }, [activeWindow, closeWindowByHwnd]);

  const minimizeSelected = useCallback(() => {
    const hwnd = activeWindow?.hwnd;
    if (hwnd) void minimizeWindowByHwnd(hwnd);
  }, [activeWindow, minimizeWindowByHwnd]);

  const closeGroupSelected = useCallback(() => {
    const group = activeGroup;
    if (!group) return;
    void (async () => {
      for (const win of group.windows) {
        await invoke("close_window", { hwnd: win.hwnd });
      }
      // 给目标应用处理关闭的时间,再刷新列表
      await new Promise((resolve) => setTimeout(resolve, 400));
      await refresh();
    })().catch((err) => {
      setError(err instanceof Error ? err.message : String(err));
    });
  }, [activeGroup, refresh]);

  const activateCurrentSelection = useCallback(async () => {
    const selection = selectedRef.current;
    const hwnd = groupsRef.current[selection.group]?.windows[selection.window]?.hwnd;
    if (!hwnd) return;
    await activateWindowByHwnd(hwnd);
  }, [activateWindowByHwnd]);

  const cycleSelection = useCallback(async () => {
    const now = performance.now();
    if (now - lastCycleAtRef.current < 80) {
      return;
    }
    lastCycleAtRef.current = now;
    sessionActiveRef.current = true;
    setError(null);

    try {
      // 缩略图缓存命中后整次枚举只要几毫秒,每次唤出都拿最新窗口列表
      let nextGroups = await invoke<AppGroup[]>("list_window_groups");
      applyGroups(nextGroups);

      applySelection(nextSelection(nextGroups, selectedRef.current));
      await getCurrentWindow().show();
      playEntrance();
      blurSearch();
      await invoke("apply_switcher_window_bounds");
      await getCurrentWindow().setFocus();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, [applyGroups, applySelection, blurSearch, nextSelection, playEntrance]);

  const cycleGroupSelection = useCallback(async () => {
    const now = performance.now();
    if (now - lastCycleAtRef.current < 80) {
      return;
    }
    lastCycleAtRef.current = now;
    sessionActiveRef.current = true;
    setError(null);

    try {
      let nextGroups = await invoke<AppGroup[]>("list_window_groups");
      applyGroups(nextGroups);

      applySelection(nextGroupSelection(nextGroups, selectedRef.current));
      await getCurrentWindow().show();
      playEntrance();
      blurSearch();
      await invoke("apply_switcher_window_bounds");
      await getCurrentWindow().setFocus();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, [applyGroups, applySelection, blurSearch, nextGroupSelection, playEntrance]);

  const loadSettings = useCallback(async () => {
    try {
      const result = await invoke<SwitcherSettings>("get_settings");
      setSettings(result);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  const saveSettings = useCallback(async () => {
    setSavingSettings(true);
    setError(null);
    try {
      await invoke("update_settings", { settings });
      setSettingsOpen(false);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingSettings(false);
    }
  }, [settings]);

  useEffect(() => {
    void loadSettings();
    void refresh({ group: 0, window: 0 });

    const unlistenOpen = listen("switcher:open", async () => {
      sessionActiveRef.current = false;
      await refresh();
      await getCurrentWindow().show();
      playEntrance();
      blurSearch();
      await invoke("apply_switcher_window_bounds");
      await getCurrentWindow().setFocus();
    });

    const unlistenCycle = listen("switcher:cycle", () => {
      void cycleSelection();
    });

    const unlistenGroupCycle = listen("switcher:group-cycle", () => {
      void cycleGroupSelection();
    });

    const unlistenCommit = listen("switcher:commit", () => {
      if (sessionActiveRef.current) {
        void activateCurrentSelection();
      }
    });

    const unlistenChanged = listen("windows:changed", () => {
      void refresh();
    });

    return () => {
      void unlistenOpen.then((dispose) => dispose());
      void unlistenCycle.then((dispose) => dispose());
      void unlistenGroupCycle.then((dispose) => dispose());
      void unlistenCommit.then((dispose) => dispose());
      void unlistenChanged.then((dispose) => dispose());
    };
  }, [activateCurrentSelection, blurSearch, cycleGroupSelection, cycleSelection, loadSettings, playEntrance, refresh]);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      // 捕获新热键:拦截所有按键,组合键写入设置,Esc 取消
      if (hotkeyCapturingRef.current) {
        event.preventDefault();
        event.stopPropagation();

        if (event.key === "Escape") {
          hotkeyCapturingRef.current = false;
          setHotkeyCapturing(false);
          setHotkeyHint(null);
          return;
        }

        if (["Alt", "Control", "Shift", "Meta"].includes(event.key)) {
          return;
        }

        const key = codeToKey(event.code);
        if (!key) {
          return;
        }

        const parts: string[] = [];
        if (event.ctrlKey) parts.push("Ctrl");
        if (event.altKey) parts.push("Alt");
        if (event.shiftKey) parts.push("Shift");
        if (event.metaKey) parts.push("Win");
        if (parts.length === 0) {
          setHotkeyHint("热键需要至少一个修饰键(Ctrl / Alt / Shift / Win)");
          return;
        }

        setSettings((current) => ({ ...current, hotkey: [...parts, key].join("+") }));
        hotkeyCapturingRef.current = false;
        setHotkeyCapturing(false);
        setHotkeyHint(null);
        return;
      }

      if (event.key === "Escape") {
        event.preventDefault();
        sessionActiveRef.current = false;
        // 通知后端停掉 Alt 松开监视线程,避免之后松开 Alt 时误提交
        void invoke("cancel_switcher_session").catch(() => {});
        void getCurrentWindow().hide();
        return;
      }

      if (event.key === "Alt") {
        return;
      }

      if (event.key === "Enter") {
        event.preventDefault();
        void activateSelected();
        return;
      }

      // W/M 窗口管理:在输入框里打字时不触发
      const target = event.target;
      const isTyping =
        target instanceof HTMLElement &&
        (target.tagName === "INPUT" || target.tagName === "TEXTAREA");

      if (!isTyping && (event.key === "w" || event.key === "W")) {
        event.preventDefault();
        if (event.shiftKey) {
          void closeGroupSelected();
        } else {
          void closeSelected();
        }
        return;
      }

      if (!isTyping && (event.key === "m" || event.key === "M")) {
        event.preventDefault();
        void minimizeSelected();
        return;
      }

      if (!isTyping && (event.key === "t" || event.key === "T") && activeWindow) {
        event.preventDefault();
        void toggleTopmostByHwnd(activeWindow.hwnd);
        return;
      }

      if (!isTyping && (event.key === "x" || event.key === "X") && activeWindow) {
        event.preventDefault();
        void maximizeRestoreByHwnd(activeWindow.hwnd);
        return;
      }

      if (!isTyping && event.ctrlKey && event.key === "ArrowRight" && activeWindow) {
        event.preventDefault();
        void moveToNextMonitorByHwnd(activeWindow.hwnd);
        return;
      }

      if (event.key === "ArrowDown") {
        event.preventDefault();
        if (!activeGroup) return;
        const next = {
          group: selectedRef.current.group,
          window: Math.min(selectedRef.current.window + 1, activeGroup.windows.length - 1)
        };
        applySelection(next);
        return;
      }

      if (event.key === "ArrowUp") {
        event.preventDefault();
        applySelection({
          group: selectedRef.current.group,
          window: Math.max(selectedRef.current.window - 1, 0)
        });
        return;
      }

      if ((event.key === "ArrowRight" || event.key === "Tab") && !event.ctrlKey) {
        event.preventDefault();
        applySelection({
          group: filteredGroups.length === 0 ? 0 : Math.min(selectedRef.current.group + 1, filteredGroups.length - 1),
          window: 0
        });
        return;
      }

      if (event.key === "ArrowLeft") {
        event.preventDefault();
        applySelection({
          group: Math.max(selectedRef.current.group - 1, 0),
          window: 0
        });
      }
    };

    window.addEventListener("keydown", handler);
    return () => {
      window.removeEventListener("keydown", handler);
    };
  }, [
    activateSelected,
    activeGroup,
    activeWindow,
    applySelection,
    closeGroupSelected,
    closeSelected,
    filteredGroups.length,
    maximizeRestoreByHwnd,
    minimizeSelected,
    moveToNextMonitorByHwnd,
    toggleTopmostByHwnd
  ]);

  // 右键菜单:点击别处或 Esc 时关闭(Esc 在捕获阶段拦截,避免同时隐藏切换器)
  useEffect(() => {
    if (!rowMenu) {
      return;
    }
    const close = () => setRowMenu(null);
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        setRowMenu(null);
      }
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [rowMenu]);

  // 滚轮切换:侧栏滚 = 换分组,详情区滚 = 换窗口(方向键的鼠标版)
  useEffect(() => {
    const sidebar = sidebarRef.current;
    const detail = detailRef.current;
    if (!sidebar || !detail) {
      return;
    }

    let lastAt = 0;
    const make = (pick: (dir: number) => void) => (event: WheelEvent) => {
      event.preventDefault();
      const now = performance.now();
      if (now - lastAt < 70) {
        return;
      }
      lastAt = now;
      pick(event.deltaY > 0 ? 1 : -1);
    };

    const onSidebar = make((dir) => {
      if (filteredGroups.length === 0) {
        return;
      }
      applySelection(
        dir > 0
          ? nextGroupSelection(filteredGroups, selectedRef.current)
          : prevGroupSelection(filteredGroups, selectedRef.current)
      );
    });
    const onDetail = make((dir) => {
      if (filteredGroups.length === 0) {
        return;
      }
      applySelection(
        dir > 0
          ? nextSelection(filteredGroups, selectedRef.current)
          : prevSelection(filteredGroups, selectedRef.current)
      );
    });

    sidebar.addEventListener("wheel", onSidebar, { passive: false });
    detail.addEventListener("wheel", onDetail, { passive: false });
    return () => {
      sidebar.removeEventListener("wheel", onSidebar);
      detail.removeEventListener("wheel", onDetail);
    };
  }, [applySelection, filteredGroups, nextGroupSelection, nextSelection, prevGroupSelection, prevSelection]);

  useEffect(() => {
    if (selectedGroup >= filteredGroups.length) {
      applySelection({ group: Math.max(filteredGroups.length - 1, 0), window: 0 });
    }
  }, [applySelection, filteredGroups.length, selectedGroup]);

  useEffect(() => {
    if (!settingsOpen && hotkeyCapturingRef.current) {
      hotkeyCapturingRef.current = false;
      setHotkeyCapturing(false);
      setHotkeyHint(null);
    }
  }, [settingsOpen]);

  // 预览伪实时:选中窗口稳定 120ms 后单独重截它的缩略图,大图区永远是最新内容
  const previewHwnd = activeWindow?.hwnd ?? null;
  useEffect(() => {
    if (!previewHwnd) {
      return;
    }
    let cancelled = false;
    const timer = window.setTimeout(() => {
      void invoke<string | null>("recapture_window_thumbnail", { hwnd: previewHwnd })
        .then((fresh) => {
          if (cancelled || !fresh) {
            return;
          }
          const mutate = (groups: AppGroup[]) =>
            groups.map((group) => ({
              ...group,
              windows: group.windows.map((win) =>
                win.hwnd === previewHwnd ? { ...win, thumbnail: fresh } : win
              ),
            }));
          groupsRef.current = mutate(groupsRef.current);
          setGroups(mutate);
        })
        .catch(() => {});
    }, 120);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [previewHwnd]);

  return (
    <main className="shell" ref={shellRef}>
      <header className="toolbar">
        <div className="brand">
          <Monitor size={18} />
          <span>Grouped Switcher</span>
        </div>
        <label className="search">
          <Search size={16} />
          <input
            ref={searchRef}
            autoFocus
            placeholder="Filter windows"
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              applySelection({ group: 0, window: 0 });
            }}
          />
        </label>
        <button className="icon-button" type="button" onClick={() => void refresh()} disabled={loading} title="Refresh">
          <RefreshCw size={17} className={loading ? "spin" : undefined} />
        </button>
        <button
          className={`icon-button ${settingsOpen ? "active" : ""}`}
          type="button"
          onClick={() => setSettingsOpen((open) => !open)}
          title="Settings"
        >
          <Settings size={17} />
        </button>
      </header>

      {error ? <div className="error">{error}</div> : null}
      {notice ? <div className="notice">{notice}</div> : null}

      {settingsOpen ? (
        <section className="settings-panel" aria-label="Settings">
          <div className="setting-group">
            <span className="setting-label">切换热键</span>
            <button
              className={`hotkey-field ${hotkeyCapturing ? "listening" : ""}`}
              type="button"
              title="点击后按下新的组合键,Esc 取消"
              onClick={() => {
                hotkeyCapturingRef.current = true;
                setHotkeyCapturing(true);
                setHotkeyHint(null);
              }}
            >
              {hotkeyCapturing ? "按下新组合键…" : settings.hotkey}
            </button>
            <span className={`setting-hint ${hotkeyHint ? "error" : ""}`}>
              {hotkeyHint ?? "组间切换 = 该热键 + Shift"}
            </span>
          </div>

          <div className="setting-group">
            <span className="setting-label">窗口大小</span>
            <div className="segmented-control" role="group" aria-label="Window size mode">
              <button
                className={settings.window_mode === "fullscreen" ? "selected" : ""}
                type="button"
                onClick={() => setSettings((current) => ({ ...current, window_mode: "fullscreen" }))}
              >
                全屏
              </button>
              <button
                className={settings.window_mode === "custom" ? "selected" : ""}
                type="button"
                onClick={() => setSettings((current) => ({ ...current, window_mode: "custom" }))}
              >
                自定义
              </button>
            </div>
          </div>

          <label className="number-field">
            <span>宽度</span>
            <input
              type="number"
              min={720}
              max={7680}
              step={10}
              disabled={settings.window_mode !== "custom"}
              value={settings.window_width}
              onChange={(event) =>
                setSettings((current) => ({
                  ...current,
                  window_width: Number(event.target.value) || current.window_width
                }))
              }
            />
          </label>

          <label className="number-field">
            <span>高度</span>
            <input
              type="number"
              min={460}
              max={4320}
              step={10}
              disabled={settings.window_mode !== "custom"}
              value={settings.window_height}
              onChange={(event) =>
                setSettings((current) => ({
                  ...current,
                  window_height: Number(event.target.value) || current.window_height
                }))
              }
            />
          </label>

          <label className="toggle-field">
            <span>开机自启</span>
            <input
              type="checkbox"
              checked={settings.auto_start}
              onChange={(event) =>
                setSettings((current) => ({
                  ...current,
                  auto_start: event.target.checked
                }))
              }
            />
          </label>

          <label className="toggle-field">
            <span>只显示当前桌面</span>
            <input
              type="checkbox"
              checked={settings.show_current_desktop_only}
              onChange={(event) =>
                setSettings((current) => ({
                  ...current,
                  show_current_desktop_only: event.target.checked
                }))
              }
            />
          </label>

          <button className="save-button" type="button" onClick={() => void saveSettings()} disabled={savingSettings}>
            <Save size={16} />
            <span>{savingSettings ? "保存中" : "保存"}</span>
          </button>
        </section>
      ) : null}

      <section className="content">
        <aside className="groups" ref={sidebarRef} aria-label="Application groups">
          {filteredGroups.map((group, groupIndex) => (
            <button
              key={group.group_id}
              className={`group ${groupIndex === selectedGroup ? "selected" : ""}`}
              type="button"
              onClick={() => applySelection({ group: groupIndex, window: 0 })}
            >
              <div className="app-icon">
                {group.icon ? (
                  <img src={group.icon} alt="" draggable={false} />
                ) : (
                  <span>{initials(group.app_name)}</span>
                )}
              </div>
              <div className="group-copy">
                <span className="group-name">{group.app_name}</span>
                <span className="group-count">{group.windows.length} windows</span>
              </div>
            </button>
          ))}
        </aside>

        <section className="windows" ref={detailRef} aria-label="Windows">
          {activeGroup ? (
            <>
              <div className="active-heading">
                <h1>{activeGroup.app_name}</h1>
                <span>{activeGroup.exe_path}</span>
              </div>
              <div className="preview-stage">
                {activeWindow?.thumbnail ? (
                  <img className="preview-image" src={activeWindow.thumbnail} alt="" />
                ) : (
                  <div className="preview-fallback">
                    <div className="preview-icon">
                      {activeGroup.icon ? (
                        <img src={activeGroup.icon} alt="" draggable={false} />
                      ) : (
                        <span>{initials(activeWindow?.process_name ?? activeGroup.app_name)}</span>
                      )}
                    </div>
                  </div>
                )}
              </div>
              <div className="window-list">
                {activeGroup.windows.map((window, windowIndex) => (
                  <button
                    key={window.hwnd}
                    className={`window-row ${windowIndex === selectedWindow ? "selected" : ""}`}
                    type="button"
                    onMouseEnter={() => applySelection({ group: selectedGroup, window: windowIndex })}
                    onClick={() => void activateWindowByHwnd(window.hwnd)}
                    onContextMenu={(event) => {
                      event.preventDefault();
                      applySelection({ group: selectedGroup, window: windowIndex });
                      setRowMenu({ x: event.clientX, y: event.clientY, hwnd: window.hwnd });
                    }}
                  >
                    {window.thumbnail ? (
                      <img className="window-preview" src={window.thumbnail} alt="" />
                    ) : (
                      <div className="window-icon">
                        {activeGroup.icon ? (
                          <img src={activeGroup.icon} alt="" draggable={false} />
                        ) : (
                          <span>{initials(window.process_name)}</span>
                        )}
                      </div>
                    )}
                    <div className="window-copy">
                      <span className="window-title">{window.title}</span>
                      <span className="window-meta">
                        PID {window.pid}
                        {window.is_minimized ? " · minimized" : ""}
                        {!window.is_on_current_desktop ? " · 其他桌面" : ""}
                      </span>
                    </div>
                  </button>
                ))}
              </div>
              <div className="shortcut-hints">
                <span>Enter 切换</span>
                <span>W 关闭</span>
                <span>Shift+W 关整组</span>
                <span>M 最小化</span>
                <span>X 最大化</span>
                <span>T 置顶</span>
                <span>Ctrl+→ 移屏</span>
                <span>Esc 退出</span>
              </div>
            </>
          ) : (
            <div className="empty">
              <Monitor size={32} />
              <span>No switchable windows found</span>
            </div>
          )}
        </section>
      </section>
      {rowMenu ? (
        <div
          className="row-menu"
          style={{
            left: Math.min(rowMenu.x, window.innerWidth - 190),
            top: Math.min(rowMenu.y, window.innerHeight - 170)
          }}
        >
          <button
            type="button"
            onClick={() => {
              void maximizeRestoreByHwnd(rowMenu.hwnd);
              setRowMenu(null);
            }}
          >
            最大化 / 还原
          </button>
          <button
            type="button"
            onClick={() => {
              void minimizeWindowByHwnd(rowMenu.hwnd);
              setRowMenu(null);
            }}
          >
            最小化
          </button>
          <button
            type="button"
            onClick={() => {
              void toggleTopmostByHwnd(rowMenu.hwnd);
              setRowMenu(null);
            }}
          >
            置顶 / 取消置顶
          </button>
          <button
            type="button"
            onClick={() => {
              void moveToNextMonitorByHwnd(rowMenu.hwnd);
              setRowMenu(null);
            }}
          >
            移动到下一显示器
          </button>
          <button
            type="button"
            className="danger"
            onClick={() => {
              void closeWindowByHwnd(rowMenu.hwnd);
              setRowMenu(null);
            }}
          >
            关闭窗口
          </button>
        </div>
      ) : null}
    </main>
  );
}
