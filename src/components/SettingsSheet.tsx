import { useEffect, useRef, useState } from "react";

const DEFAULT_SHOW = "CmdOrCtrl+Shift+Space";
const DEFAULT_CAPTURE = "CmdOrCtrl+Alt+C";

interface Props {
  showShortcut: string;
  captureShortcut: string;
  onSave: (showShortcut: string, captureShortcut: string) => Promise<void>;
  onClose: () => void;
}

function recordedShortcut(event: React.KeyboardEvent<HTMLInputElement>) {
  const modifierKey = ["Meta", "Control", "Alt", "Shift"].includes(event.key);
  if (modifierKey) return null;

  let key = event.key;
  if (event.code === "Space") key = "Space";
  else if (event.code.startsWith("Key")) key = event.code.slice(3);
  else if (event.code.startsWith("Digit")) key = event.code.slice(5);
  else if (key.length === 1) key = key.toUpperCase();

  const parts: string[] = [];
  if (event.metaKey) parts.push("Cmd");
  if (event.ctrlKey) parts.push("Control");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");
  if (!parts.length) return "";
  parts.push(key);
  return parts.join("+");
}

function ShortcutField({
  label,
  value,
  onChange,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
}) {
  const [recording, setRecording] = useState(false);
  return (
    <label className="settings-row">
      <span>{label}</span>
      <input
        className={`shortcut-recorder${recording ? " recording" : ""}`}
        value={recording ? "Press shortcut…" : value}
        readOnly
        spellCheck={false}
        onFocus={() => setRecording(true)}
        onBlur={() => setRecording(false)}
        onKeyDown={(event) => {
          event.preventDefault();
          event.stopPropagation();
          if (event.key === "Escape") {
            event.currentTarget.blur();
            return;
          }
          const shortcut = recordedShortcut(event);
          if (shortcut === null) return;
          if (shortcut) onChange(shortcut);
          event.currentTarget.blur();
        }}
      />
    </label>
  );
}

export default function SettingsSheet({
  showShortcut: initialShow,
  captureShortcut: initialCapture,
  onSave,
  onClose,
}: Props) {
  const closeRef = useRef<HTMLButtonElement>(null);
  const [showShortcut, setShowShortcut] = useState(initialShow);
  const [captureShortcut, setCaptureShortcut] = useState(initialCapture);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    closeRef.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !(event.target as HTMLElement).classList.contains("shortcut-recorder")) {
        event.preventDefault();
        event.stopImmediatePropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [onClose]);

  const save = async () => {
    if (showShortcut === captureShortcut) {
      setError("Choose a different shortcut for each action.");
      return;
    }
    setSaving(true);
    setError(null);
    try {
      await onSave(showShortcut, captureShortcut);
      onClose();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="overlay settings-overlay" onMouseDown={onClose}>
      <div
        className="sheet settings-sheet"
        role="dialog"
        aria-modal="true"
        aria-labelledby="settings-title"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <div className="sheet-heading">
          <div>
            <div id="settings-title" className="sheet-title">Settings</div>
            <div className="settings-subtitle">Global shortcuts</div>
          </div>
          <button ref={closeRef} className="sheet-close" aria-label="Close settings" onClick={onClose}>×</button>
        </div>
        <ShortcutField label="Show or hide Clippy" value={showShortcut} onChange={setShowShortcut} />
        <ShortcutField label="Capture selected text" value={captureShortcut} onChange={setCaptureShortcut} />
        <p className="settings-hint">Click a shortcut, then press the new key combination. Double Shift capture stays available.</p>
        {error && <div className="settings-error" role="alert">{error}</div>}
        <div className="settings-actions">
          <button
            className="settings-reset"
            onClick={() => {
              setShowShortcut(DEFAULT_SHOW);
              setCaptureShortcut(DEFAULT_CAPTURE);
              setError(null);
            }}
          >
            Restore defaults
          </button>
          <button className="settings-save" disabled={saving} onClick={() => void save()}>
            {saving ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
}
