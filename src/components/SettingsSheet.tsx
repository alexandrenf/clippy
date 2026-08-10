import { useEffect, useRef, useState } from "react";
import { useMotionExit } from "../motion";
import { api } from "../store";

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
  const [syncEnvironment, setSyncEnvironment] = useState<"staging" | "production">("production");
  const [syncConnected, setSyncConnected] = useState(false);
  const [signInBusy, setSignInBusy] = useState(false);
  const [pairing, setPairing] = useState<{ payload: string; expiresAtMs: number } | null>(null);
  const [pairingBusy, setPairingBusy] = useState(false);
  const [pairingError, setPairingError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [agentEnabled, setAgentEnabled] = useState(false);
  const [agentBusy, setAgentBusy] = useState(false);
  const [agentError, setAgentError] = useState<string | null>(null);
  const { isExiting, requestExit } = useMotionExit(onClose);

  useEffect(() => {
    closeRef.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !(event.target as HTMLElement).classList.contains("shortcut-recorder")) {
        event.preventDefault();
        event.stopImmediatePropagation();
        requestExit();
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [requestExit]);

  useEffect(() => {
    let cancelled = false;
    setSyncConnected(false);
    void api.syncAuthStatus(syncEnvironment).then((connected) => {
      if (!cancelled) setSyncConnected(connected);
    });
    return () => {
      cancelled = true;
    };
  }, [syncEnvironment]);

  useEffect(() => {
    let cancelled = false;
    void api.agentCompanionStatus().then((enabled) => {
      if (!cancelled) setAgentEnabled(enabled);
    }).catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, []);

  const save = async () => {
    if (showShortcut === captureShortcut) {
      setError("Choose a different shortcut for each action.");
      return;
    }
    setSaving(true);
    setError(null);
    try {
      await onSave(showShortcut, captureShortcut);
      requestExit();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div
      className={`overlay settings-overlay${isExiting ? " is-closing" : ""}`}
      onMouseDown={requestExit}
    >
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
          <button ref={closeRef} className="sheet-close" aria-label="Close settings" onClick={requestExit}>×</button>
        </div>
        <ShortcutField label="Show or hide Clippy" value={showShortcut} onChange={setShowShortcut} />
        <ShortcutField label="Capture selected text" value={captureShortcut} onChange={setCaptureShortcut} />
        <p className="settings-hint">Click a shortcut, then press the new key combination. Double Shift capture stays available.</p>
        <div className="settings-subtitle">Device sync</div>
        <label className="settings-row">
          <span>Environment</span>
          <select
            className="shortcut-recorder"
            value={syncEnvironment}
            onChange={(event) => {
              setSyncEnvironment(event.target.value as "staging" | "production");
              setPairing(null);
              setPairingError(null);
              setSyncConnected(false);
            }}
          >
            <option value="production">Production</option>
            <option value="staging">Staging</option>
          </select>
        </label>
        <p className="settings-hint">
          Sign in in your browser, then pair the iPhone companion with an end-to-end encrypted workspace.
          The browser redirects back to Clippy automatically; there is no code to copy.
        </p>
        <div className="settings-actions">
          <button
            className="settings-save"
            disabled={signInBusy || syncConnected}
            onClick={() => {
              setSignInBusy(true);
              setPairingError(null);
              void api.signInSync(syncEnvironment)
                .then(() => setSyncConnected(true))
                .catch((cause) => setPairingError(cause instanceof Error ? cause.message : String(cause)))
                .finally(() => setSignInBusy(false));
            }}
          >
            {signInBusy ? "Connecting…" : syncConnected ? "Signed in" : "Sign in"}
          </button>
          <button
            className="settings-reset"
            disabled={pairingBusy || signInBusy || !syncConnected}
            onClick={() => {
              setPairingBusy(true);
              setPairingError(null);
              setCopied(false);
              void api.beginSyncPairing(syncEnvironment)
                .then((result) => setPairing(result))
                .catch((cause) => setPairingError(cause instanceof Error ? cause.message : String(cause)))
                .finally(() => setPairingBusy(false));
            }}
          >
            {pairingBusy ? "Preparing…" : "Pair iPhone"}
          </button>
          {pairing && (
            <button
              className="settings-save"
              onClick={() => {
                void api.copyText(pairing.payload).then(() => setCopied(true));
              }}
            >
              {copied ? "Copied" : "Copy offer"}
            </button>
          )}
        </div>
        {pairing && (
          <input
            className="shortcut-recorder"
            readOnly
            aria-label="Encrypted iPhone pairing offer"
            value={pairing.payload}
            onFocus={(event) => event.currentTarget.select()}
          />
        )}
        {pairingError && <div className="settings-error" role="alert">{pairingError}</div>}
        <div className="settings-subtitle">Agent access</div>
        <p className="settings-hint">
          Optionally let local Codex agents use your Clippy lists. Access starts read-only;
          you can ask the agent to change its local policy later.
        </p>
        <div className="settings-actions">
          <button
            className="settings-save"
            disabled={agentBusy || agentEnabled}
            onClick={() => {
              setAgentBusy(true);
              setAgentError(null);
              void api.installAgentCompanion()
                .then(() => setAgentEnabled(true))
                .catch((cause) => setAgentError(cause instanceof Error ? cause.message : String(cause)))
                .finally(() => setAgentBusy(false));
            }}
          >
            {agentBusy ? "Enabling…" : agentEnabled ? "Agent access enabled" : "Enable agent access"}
          </button>
        </div>
        {agentError && <div className="settings-error" role="alert">{agentError}</div>}
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
