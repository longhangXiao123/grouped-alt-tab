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
  const groupsRef = useRef<AppGroup[]>(emptyGroups);
  const selectedRef = useRef<Selection>({ group: 0, window: 0 });
  const sessionActiveRef = useRef(false);
  const lastCycleAtRef = useRef(0);

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

  const activateCurrentSelection = useCallback(async () => {
    const selection = selectedRef.current;
    const hwnd = groupsRef.current[selection.group]?.windows[selection.window]?.hwnd;
    if (!hwnd) return;
    await activateWindowByHwnd(hwnd);
  }, [activateWindowByHwnd]);

  const cycleSelection = useCallback(async (forceRefresh: boolean) => {
    const now = performance.now();
    if (now - lastCycleAtRef.current < 80) {
      return;
    }
    lastCycleAtRef.current = now;
    sessionActiveRef.current = true;
    setError(null);

    try {
      let nextGroups = groupsRef.current;
      if (forceRefresh || nextGroups.length === 0) {
        setLoading(true);
        nextGroups = await invoke<AppGroup[]>("list_window_groups");
        applyGroups(nextGroups);
      }

      applySelection(nextSelection(nextGroups, selectedRef.current));
      await getCurrentWindow().show();
      await getCurrentWindow().setFocus();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, [applyGroups, applySelection, nextSelection]);

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
      await getCurrentWindow().setFocus();
    });

    const unlistenCycle = listen("switcher:cycle", () => {
      void cycleSelection(groupsRef.current.length === 0);
    });

    const unlistenChanged = listen("windows:changed", () => {
      void refresh();
    });

    return () => {
      void unlistenOpen.then((dispose) => dispose());
      void unlistenCycle.then((dispose) => dispose());
      void unlistenChanged.then((dispose) => dispose());
    };
  }, [cycleSelection, loadSettings, refresh]);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        sessionActiveRef.current = false;
        void getCurrentWindow().hide();
        return;
      }

      if (event.key === "Alt") {
        return;
      }

      if (event.altKey && (event.key === "`" || event.code === "Backquote")) {
        event.preventDefault();
        void cycleSelection(false);
        return;
      }

      if (event.key === "Enter") {
        event.preventDefault();
        void activateSelected();
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

      if (event.key === "ArrowRight" || event.key === "Tab") {
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

    const keyUpHandler = (event: KeyboardEvent) => {
      if (event.key === "Alt" && sessionActiveRef.current) {
        event.preventDefault();
        void activateCurrentSelection();
      }
    };

    window.addEventListener("keydown", handler);
    window.addEventListener("keyup", keyUpHandler);
    return () => {
      window.removeEventListener("keydown", handler);
      window.removeEventListener("keyup", keyUpHandler);
    };
  }, [activateCurrentSelection, activateSelected, activeGroup, applySelection, cycleSelection, filteredGroups.length]);

  useEffect(() => {
    if (selectedGroup >= filteredGroups.length) {
      applySelection({ group: Math.max(filteredGroups.length - 1, 0), window: 0 });
    }
  }, [applySelection, filteredGroups.length, selectedGroup]);

  return (
    <main className="shell">
      <header className="toolbar">
        <div className="brand">
          <Monitor size={18} />
          <span>Grouped Switcher</span>
        </div>
        <label className="search">
          <Search size={16} />
          <input
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

      {settingsOpen ? (
        <section className="settings-panel" aria-label="Settings">
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

          <button className="save-button" type="button" onClick={() => void saveSettings()} disabled={savingSettings}>
            <Save size={16} />
            <span>{savingSettings ? "保存中" : "保存"}</span>
          </button>
        </section>
      ) : null}

      <section className="content">
        <aside className="groups" aria-label="Application groups">
          {filteredGroups.map((group, groupIndex) => (
            <button
              key={group.group_id}
              className={`group ${groupIndex === selectedGroup ? "selected" : ""}`}
              type="button"
              onClick={() => applySelection({ group: groupIndex, window: 0 })}
            >
              <div className="app-icon">{initials(group.app_name)}</div>
              <div className="group-copy">
                <span className="group-name">{group.app_name}</span>
                <span className="group-count">{group.windows.length} windows</span>
              </div>
            </button>
          ))}
        </aside>

        <section className="windows" aria-label="Windows">
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
                    <div className="preview-icon">{initials(activeWindow?.process_name ?? activeGroup.app_name)}</div>
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
                  >
                    {window.thumbnail ? (
                      <img className="window-preview" src={window.thumbnail} alt="" />
                    ) : (
                      <div className="window-icon">{initials(window.process_name)}</div>
                    )}
                    <div className="window-copy">
                      <span className="window-title">{window.title}</span>
                      <span className="window-meta">
                        PID {window.pid}
                        {window.is_minimized ? " · minimized" : ""}
                      </span>
                    </div>
                  </button>
                ))}
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
    </main>
  );
}
