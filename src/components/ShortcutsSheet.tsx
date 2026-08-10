import { useEffect, useRef } from "react";
import { useMotionExit } from "../motion";

const IS_MAC = navigator.userAgent.includes("Mac");
const MOD = IS_MAC ? "⌘" : "Ctrl";
const DELETE_KEY = IS_MAC ? "⌫" : "Del";

const IN_APP_ROWS: [string, string[]][] = [
  ["Switch list", [`${MOD} K`]],
  ["Search", [`${MOD} F`]],
  ["Select all", [`${MOD} A`]],
  ["Copy selected", [`${MOD} C`]],
  ["Copy as list", [`${MOD} ⇧ C`]],
  ["Merge selected", [`${MOD} ⇧ M`]],
  ["Mark as done", ["Space"]],
  ["Edit", ["Enter"]],
  ["Edit in new window", [`${MOD} Enter`]],
  ["Delete", [DELETE_KEY]],
  ["Navigate", ["↑", "↓"]],
  ["This sheet", [`${MOD} /`]],
  ["Hide panel", ["Esc"]],
  ["Hide window", [`${MOD} W`]],
];

interface Props {
  showShortcut: string;
  captureShortcut: string;
  onClose: () => void;
}

export default function ShortcutsSheet({ showShortcut, captureShortcut, onClose }: Props) {
  const groups: { title: string; rows: [string, string[]][] }[] = [
    {
      title: "Anywhere",
      rows: [
        ["Capture selected text", ["Left Shift", "Left Shift"]],
        ["Show / hide Clippy", ["Right Shift", "Right Shift"]],
        ["Show / hide (fallback)", [showShortcut]],
        ["Capture (fallback)", [captureShortcut]],
      ],
    },
    { title: "In Clippy", rows: IN_APP_ROWS },
  ];
  const closeRef = useRef<HTMLButtonElement>(null);
  const { isExiting, requestExit } = useMotionExit(onClose);

  useEffect(() => {
    closeRef.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopImmediatePropagation();
        requestExit();
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [requestExit]);

  return (
    <div className={`overlay${isExiting ? " is-closing" : ""}`} onMouseDown={requestExit}>
      <div
        className="sheet"
        role="dialog"
        aria-modal="true"
        aria-labelledby="shortcuts-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="sheet-heading">
          <div id="shortcuts-title" className="sheet-title">Shortcuts</div>
          <button ref={closeRef} className="sheet-close" aria-label="Close shortcuts" onClick={requestExit}>×</button>
        </div>
        {groups.map((g) => (
          <div key={g.title} className="sheet-group">
            <div className="sheet-group-title">{g.title}</div>
            {g.rows.map(([label, keys]) => (
              <div key={label} className="sheet-row">
                <span>{label}</span>
                <span className="sheet-keys">
                  {keys.map((k, i) => (
                    <kbd key={i}>{k}</kbd>
                  ))}
                </span>
              </div>
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}
