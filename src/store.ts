import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
import type { AppState, AttachmentDraft, Theme } from "./types";

function messageFrom(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

export const api = {
  getState: () => invoke<AppState>("get_state"),
  addEntry: (content: string, attachmentPaths: string[] = []) =>
    invoke<void>("add_entry", { content, attachmentPaths }),
  inspectAttachments: (paths: string[]) =>
    invoke<AttachmentDraft[]>("inspect_attachments", { paths }),
  pasteClipboardImage: () => invoke<AttachmentDraft>("paste_clipboard_image"),
  discardPastedImage: (path: string) =>
    invoke<void>("discard_pasted_image", { path }),
  getAttachmentPreview: (id: number) =>
    invoke<string | null>("get_attachment_preview", { id }),
  openAttachment: (id: number) => invoke<void>("open_attachment", { id }),
  startFileDrag: (paths: string[], image: string) => {
    const onEvent = new Channel<{ result: string; cursorPos: { x: number; y: number } }>();
    return invoke<void>("plugin:drag|start_drag", {
      item: paths,
      image,
      options: { mode: "copy" },
      onEvent,
    });
  },
  startTextDrag: (text: string, image: string) => {
    const onEvent = new Channel<{ result: string; cursorPos: { x: number; y: number } }>();
    return invoke<void>("plugin:drag|start_drag", {
      item: {
        data: text,
        types: ["public.utf8-plain-text", "text/plain", "UTF8_STRING"],
      },
      image,
      options: { mode: "copy" },
      onEvent,
    });
  },
  setItemsDone: (ids: number[], done: boolean) =>
    invoke<void>("set_items_done", { ids, done }),
  updateItem: (id: number, content: string) => invoke<void>("update_item", { id, content }),
  deleteItems: (ids: number[]) => invoke<void>("delete_items", { ids }),
  mergeItems: (ids: number[]) => invoke<void>("merge_items", { ids }),
  moveItems: (ids: number[], sectionId: number | null) =>
    invoke<void>("move_items", { ids, sectionId }),
  clearCompleted: () => invoke<void>("clear_completed"),
  setActiveSection: (id: number | null) => invoke<void>("set_active_section", { id }),
  createSection: (name: string) => invoke<number>("create_section", { name }),
  renameSection: (id: number, name: string) => invoke<void>("rename_section", { id, name }),
  deleteSection: (id: number, deleteItems = false) =>
    invoke<void>("delete_section", { id, deleteItems }),
  setTheme: (theme: Theme) => invoke<void>("set_theme", { theme }),
  setKeepOnTop: (enabled: boolean) => invoke<void>("set_keep_on_top", { enabled }),
  setShortcuts: (showShortcut: string, captureShortcut: string) =>
    invoke<void>("set_shortcuts", { showShortcut, captureShortcut }),
  copyText: (text: string, paths: string[] = []) =>
    invoke<void>("copy_text", { text, paths }),
  exportMarkdown: () => invoke<string>("export_markdown"),
  revealNotes: () => invoke<void>("reveal_notes"),
  checkForUpdates: () => invoke<void>("check_for_updates"),
  captureNow: () => invoke<void>("capture_now"),
  hidePanel: () => invoke<void>("hide_panel"),
  openEditor: (id: number) => invoke<void>("open_editor", { id }),
  accessibilityStatus: () => invoke<boolean>("accessibility_status"),
  requestAccessibilityPermission: () => invoke<boolean>("request_accessibility_permission"),
  openAccessibilitySettings: () => invoke<void>("open_accessibility_settings"),
  agentCompanionStatus: () => invoke<boolean>("agent_companion_status"),
  installAgentCompanion: () => invoke<void>("install_agent_companion"),
  signInSync: (environment: "staging" | "production" = "production") =>
    invoke<{ environment: string; endpoint: string }>("sign_in_sync", { environment }),
  signOutSync: (environment: "staging" | "production" = "production") =>
    invoke<void>("sign_out_sync", { environment }),
  syncAuthStatus: (environment: "staging" | "production" = "production") =>
    invoke<boolean>("sync_auth_status", { environment }),
  syncStatus: () =>
    invoke<"idle" | "syncing" | "synced" | "waitingForDevice">("sync_status"),
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
