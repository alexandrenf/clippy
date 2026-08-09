import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect, useRef, useState } from "react";
import { api, applyTheme } from "../store";

const IS_MAC = navigator.userAgent.includes("Mac");
const MOD = IS_MAC ? "⌘" : "Ctrl";

export default function Editor({ id }: { id: number }) {
  const [draft, setDraft] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const taRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    void api
      .getState()
      .then((state) => {
        applyTheme(state.theme);
        const item = state.items.find((i) => i.id === id);
        if (item) setDraft(item.content);
        else setError("This note no longer exists.");
      })
      .catch((cause) => setError(cause instanceof Error ? cause.message : String(cause)));
  }, [id]);

  useEffect(() => {
    if (draft !== null) taRef.current?.focus();
  }, [draft !== null]);

  const close = () => void getCurrentWindow().close();
  const save = async () => {
    if (draft === null) return;
    if (!draft.trim()) {
      setError("A note cannot be empty.");
      return;
    }
    try {
      await api.updateItem(id, draft);
      close();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };

  return (
    <div className="editor-window">
      <div className="editor-bar" data-tauri-drag-region>
        <span className="editor-title" data-tauri-drag-region>
          Edit
        </span>
        <button className="icon-btn" title="Close" aria-label="Close editor" onClick={close}>
          ✕
        </button>
      </div>
      <textarea
        ref={taRef}
        className="editor-area"
        value={draft ?? ""}
        placeholder="Loading…"
        aria-label="Note content"
        disabled={draft === null}
        onChange={(e) => {
          setDraft(e.target.value);
          if (error) setError(null);
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
            e.preventDefault();
            save();
          } else if (e.key === "Escape") {
            close();
          }
        }}
      />
      {error && <div className="editor-error" role="alert">{error}</div>}
      <div className="editor-actions">
        <span className="editor-hint">{MOD}+Enter to save · Esc to cancel</span>
        <button className="btn" onClick={close}>
          Cancel
        </button>
        <button className="btn primary" onClick={save} disabled={draft === null}>
          Save
        </button>
      </div>
    </div>
  );
}
