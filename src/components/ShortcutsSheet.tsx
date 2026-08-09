import { useEffect, useRef } from "react";

const IS_MAC = navigator.userAgent.includes("Mac");
const MOD = IS_MAC ? "⌘" : "Ctrl";
const DELETE_KEY = IS_MAC ? "⌫" : "Del";

const GROUPS: { title: string; rows: [string, string[]][] }[] = [
  {
    title: "Anywhere",
    rows: [
      ["Capture selected text", ["Left Shift", "Left Shift"]],
      ["Show / hide Cooper", ["Right Shift", "Right Shift"]],
      ["Show / hide (fallback)", [`${MOD} Shift Space`]],
      ["Capture (fallback)", [IS_MAC ? `${MOD} ⌥ C` : `${MOD} Alt C`]],
    ],
  },
  {
    title: "In Cooper",
    rows: [
      ["Switch section", [`${MOD} K`]],
      ["Search", [`${MOD} F`]],
      ["Select all", [`${MOD} A`]],
      ["Copy selected", [`${MOD} C`]],
      ["Copy as list", [`${MOD} ⇧ C`]],
      ["Mark as done", ["Space"]],
      ["Edit", ["Enter"]],
      ["Edit in new window", [`${MOD} Enter`]],
      ["Delete", [DELETE_KEY]],
      ["Navigate", ["↑", "↓"]],
      ["This sheet", [`${MOD} /`]],
      ["Hide panel", ["Esc"]],
      ["Hide window", [`${MOD} W`]],
    ],
  },
];

export default function ShortcutsSheet({ onClose }: { onClose: () => void }) {
  const closeRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    closeRef.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopImmediatePropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [onClose]);

  return (
    <div className="overlay" onMouseDown={onClose}>
      <div
        className="sheet"
        role="dialog"
        aria-modal="true"
        aria-labelledby="shortcuts-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="sheet-heading">
          <div id="shortcuts-title" className="sheet-title">Shortcuts</div>
          <button ref={closeRef} className="sheet-close" aria-label="Close shortcuts" onClick={onClose}>×</button>
        </div>
        {GROUPS.map((g) => (
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
