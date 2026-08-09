import { useEffect, useRef, useState } from "react";

export interface MenuEntry {
  label: string;
  kbd?: string;
  danger?: boolean;
  disabled?: boolean;
  checked?: boolean;
  separatorBefore?: boolean;
  onClick: () => void;
}

interface Props {
  x: number;
  y: number;
  entries: MenuEntry[];
  onClose: () => void;
}

export default function ContextMenu({ x, y, entries, onClose }: Props) {
  const ref = useRef<HTMLDivElement>(null);
  const [activeIndex, setActiveIndex] = useState(() =>
    Math.max(0, entries.findIndex((entry) => !entry.disabled)),
  );

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const nx = Math.min(x, window.innerWidth - rect.width - 8);
    const ny = Math.min(y, window.innerHeight - rect.height - 8);
    el.style.left = `${Math.max(8, nx)}px`;
    el.style.top = `${Math.max(8, ny)}px`;
    el.querySelector<HTMLButtonElement>(`[data-menu-index="${activeIndex}"]`)?.focus();
  }, [activeIndex, x, y]);

  useEffect(() => {
    const close = () => onClose();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopImmediatePropagation();
        onClose();
        return;
      }
      const enabled = entries
        .map((entry, index) => (!entry.disabled ? index : -1))
        .filter((index) => index >= 0);
      if (!enabled.length) return;
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const current = Math.max(0, enabled.indexOf(activeIndex));
        const delta = e.key === "ArrowDown" ? 1 : -1;
        const next = (current + delta + enabled.length) % enabled.length;
        setActiveIndex(enabled[next]);
      } else if (e.key === "Home" || e.key === "End") {
        e.preventDefault();
        setActiveIndex(e.key === "Home" ? enabled[0] : enabled[enabled.length - 1]);
      } else if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        const entry = entries[activeIndex];
        if (entry && !entry.disabled) {
          onClose();
          entry.onClick();
        }
      }
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [activeIndex, entries, onClose]);

  return (
    <div
      ref={ref}
      className="context-menu"
      role="menu"
      style={{ left: x, top: y }}
      onMouseDown={(e) => e.stopPropagation()}
      onContextMenu={(e) => e.preventDefault()}
    >
      {entries.map((entry, i) => (
        <div key={i}>
          {entry.separatorBefore && <div className="menu-sep" />}
          <button
            data-menu-index={i}
            className={`menu-item${entry.danger ? " danger" : ""}`}
            role={entry.checked === undefined ? "menuitem" : "menuitemradio"}
            aria-checked={entry.checked}
            disabled={entry.disabled}
            tabIndex={i === activeIndex ? 0 : -1}
            onMouseEnter={() => !entry.disabled && setActiveIndex(i)}
            onClick={() => {
              onClose();
              entry.onClick();
            }}
          >
            <span className="menu-label">
              {entry.checked !== undefined && (
                <span className="menu-check" aria-hidden>{entry.checked ? "✓" : ""}</span>
              )}
              {entry.label}
            </span>
            {entry.kbd && <span className="menu-kbd">{entry.kbd}</span>}
          </button>
        </div>
      ))}
    </div>
  );
}
