export type WindowInfo = {
  hwnd: string;
  pid: number;
  title: string;
  process_name: string;
  exe_path: string;
  icon: string | null;
  thumbnail: string | null;
  is_minimized: boolean;
  last_active_rank: number;
};

export type AppGroup = {
  group_id: string;
  app_name: string;
  exe_path: string;
  icon: string | null;
  windows: WindowInfo[];
  active_window_hwnd: string | null;
};

export type SwitcherSettings = {
  hotkey: string;
  group_by: "exe_path";
  preview_mode: "icons" | "thumbnails";
  window_mode: "fullscreen" | "custom";
  window_width: number;
  window_height: number;
  auto_start: boolean;
  excluded_processes: string[];
};
