import { useEffect, useMemo, useRef, useState } from "react";
import type { Section } from "../types";

interface Row {
  id: number | null;
  label: string;
  create?: boolean;
}

interface Props {
  sections: Section[];
  activeSectionId: number | null;
  onSelect: (id: number | null) => void;
  onCreate: (name: string) => void;
  onClose: () => void;
  mode?: "switch" | "move";
}

export default function SectionSwitcher({
  sections,
  activeSectionId,
  onSelect,
  onCreate,
  onClose,
  mode = "switch",
}: Props) {
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => inputRef.current?.focus(), []);

  const rows = useMemo<Row[]>(() => {
    const q = query.trim().toLowerCase();
    const list: Row[] = [
      { id: null, label: "Inbox (unfiled)" },
      ...sections.map((s) => ({ id: s.id, label: s.name })),
    ].filter((r) => !q || r.label.toLowerCase().includes(q));
    if (q && !sections.some((s) => s.name.toLowerCase() === q)) {
      list.push({ id: -1, label: `Create list “${query.trim()}”`, create: true });
    }
    return list;
  }, [query, sections]);

  useEffect(() => {
    setIndex((i) => Math.min(i, Math.max(0, rows.length - 1)));
  }, [rows.length]);

  const choose = (row: Row) => {
    if (row.create) onCreate(query.trim());
    else onSelect(row.id);
    onClose();
  };

  return (
    <div className="overlay" onMouseDown={onClose}>
      <div
        className="switcher"
        role="dialog"
        aria-modal="true"
        aria-label={mode === "move" ? "Move prompts to a list" : "Switch or create a list"}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <input
          ref={inputRef}
          className="switcher-input"
          aria-label="Find a list"
          placeholder={mode === "move" ? "Move to a list…" : "Switch or create a list…"}
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setIndex(0);
          }}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setIndex((i) => Math.min(i + 1, rows.length - 1));
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setIndex((i) => Math.max(i - 1, 0));
            } else if (e.key === "Enter") {
              e.preventDefault();
              if (rows[index]) choose(rows[index]);
            } else if (e.key === "Escape") {
              onClose();
            }
          }}
        />
        <div className="switcher-list" role="listbox" aria-label="Lists">
          {rows.map((row, i) => (
            <button
              key={`${row.id}-${row.label}`}
              className={`switcher-row${i === index ? " active" : ""}${row.create ? " create" : ""}`}
              role="option"
              aria-selected={!row.create && row.id === activeSectionId}
              onMouseEnter={() => setIndex(i)}
              onClick={() => choose(row)}
            >
              <span className="switcher-label">{row.label}</span>
              {!row.create && row.id === activeSectionId && (
                <span className="switcher-check">✓</span>
              )}
            </button>
          ))}
          {rows.length === 0 && <div className="switcher-empty">No lists</div>}
        </div>
        <div className="switcher-hint">
          {mode === "move" ? (
            <>Choose a destination or type a name to create one</>
          ) : (
            <>
              New prompts go to the selected list · Type <b>## Title</b> in the composer to
              create one inline
            </>
          )}
        </div>
      </div>
    </div>
  );
}
