import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
import type { AppState, Theme } from "./types";

function messageFrom(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

export const api = {
  getState: () => invoke<AppState>("get_state"),
  addEntry: (content: string) => invoke<void>("add_entry", { content }),
  setItemsDone: (ids: number[], done: boolean) =>
    invoke<void>("set_items_done", { ids, done }),
  updateItem: (id: number, content: string) => invoke<void>("update_item", { id, content }),
  deleteItems: (ids: number[]) => invoke<void>("delete_items", { ids }),
  clearCompleted: () => invoke<void>("clear_completed"),
  setActiveSection: (id: number | null) => invoke<void>("set_active_section", { id }),
  createSection: (name: string) => invoke<number>("create_section", { name }),
  renameSection: (id: number, name: string) => invoke<void>("rename_section", { id, name }),
  deleteSection: (id: number) => invoke<void>("delete_section", { id }),
  setTheme: (theme: Theme) => invoke<void>("set_theme", { theme }),
  copyText: (text: string) => invoke<void>("copy_text", { text }),
  exportMarkdown: () => invoke<string>("export_markdown"),
  captureNow: () => invoke<void>("capture_now"),
  hidePanel: () => invoke<void>("hide_panel"),
  openEditor: (id: number) => invoke<void>("open_editor", { id }),
  accessibilityStatus: () => invoke<boolean>("accessibility_status"),
  requestAccessibilityPermission: () => invoke<boolean>("request_accessibility_permission"),
  openAccessibilitySettings: () => invoke<void>("open_accessibility_settings"),
};

export function applyTheme(theme: Theme) {
  const dark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  const root = document.documentElement;
  // "glass" follows the system light/dark preference for text colors and adds
  // a translucent backdrop (native acrylic/vibrancy where the OS provides it).
  root.dataset.theme =
    theme === "light" || theme === "dark" ? theme : dark ? "dark" : "light";
  if (theme === "glass") root.dataset.glass = "1";
  else delete root.dataset.glass;
}

export function useAppState() {
  const [state, setState] = useState<AppState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const fetching = useRef(false);
  const queued = useRef(false);
  const mounted = useRef(true);

  const refresh = useCallback(() => {
    if (fetching.current) {
      queued.current = true;
      return;
    }

    fetching.current = true;
    void (async () => {
      do {
        queued.current = false;
        try {
          const next = await api.getState();
          if (mounted.current) {
            setState(next);
            setError(null);
          }
        } catch (cause) {
          if (mounted.current) setError(messageFrom(cause));
        }
      } while (mounted.current && queued.current);
      fetching.current = false;
    })();
  }, []);

  useEffect(() => {
    mounted.current = true;
    refresh();
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen("refresh", refresh)
      .then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      })
      .catch((cause) => {
        if (!disposed) setError(messageFrom(cause));
      });
    return () => {
      disposed = true;
      mounted.current = false;
      unlisten?.();
    };
  }, [refresh]);

  useEffect(() => {
    if (!state) return;
    applyTheme(state.theme);
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => applyTheme(state.theme);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, [state?.theme]);

  return { state, error, refresh };
}
