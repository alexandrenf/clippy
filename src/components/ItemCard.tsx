import { memo, useEffect, useRef, useState } from "react";
import { renderInline } from "../markdown";
import type { Item } from "../types";

interface Props {
  item: Item;
  selected: boolean;
  editing: boolean;
  onClick: (e: React.MouseEvent, id: number) => void;
  onContextMenu: (e: React.MouseEvent, id: number) => void;
  onToggleDone: (id: number) => void;
  onEditSave: (id: number, content: string) => void | Promise<void>;
  onEditCancel: () => void;
  onDoubleClick: (id: number) => void;
}

function ItemCard({
  item,
  selected,
  editing,
  onClick,
  onContextMenu,
  onToggleDone,
  onEditSave,
  onEditCancel,
  onDoubleClick,
}: Props) {
  const [draft, setDraft] = useState(item.content);
  const taRef = useRef<HTMLTextAreaElement>(null);
  const savingRef = useRef(false);

  useEffect(() => {
    if (editing) {
      setDraft(item.content);
      const ta = taRef.current;
      if (ta) {
        ta.focus();
        ta.setSelectionRange(ta.value.length, ta.value.length);
        ta.style.height = "auto";
        ta.style.height = `${Math.min(ta.scrollHeight, 200)}px`;
      }
    }
  }, [editing, item.content]);

  const save = async () => {
    if (savingRef.current) return;
    savingRef.current = true;
    try {
      await onEditSave(item.id, draft);
    } finally {
      savingRef.current = false;
    }
  };

  const classes = ["card"];
  if (selected) classes.push("selected");
  if (item.done) classes.push("done");

  return (
    <div
      className={classes.join(" ")}
      data-item-id={item.id}
      role="option"
      aria-selected={selected}
      aria-label={`${item.done ? "Completed" : "Open"} note: ${item.content}`}
      onClick={(event) => onClick(event, item.id)}
      onContextMenu={(event) => onContextMenu(event, item.id)}
      onDoubleClick={() => onDoubleClick(item.id)}
    >
      <button
        className={`check${item.done ? " checked" : ""}`}
        title={item.done ? "Mark as not done" : "Mark as done"}
        aria-label={item.done ? "Mark as not done" : "Mark as done"}
        aria-pressed={item.done}
        onClick={(e) => {
          e.stopPropagation();
          onToggleDone(item.id);
        }}
      >
        {item.done && (
          <svg viewBox="0 0 12 12" width="10" height="10" aria-hidden>
            <path
              d="M2.5 6.5 L5 9 L9.5 3.5"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.8"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        )}
      </button>
      {editing ? (
        <textarea
          ref={taRef}
          className="card-edit"
          value={draft}
          onChange={(e) => {
            setDraft(e.target.value);
            e.target.style.height = "auto";
            e.target.style.height = `${Math.min(e.target.scrollHeight, 200)}px`;
          }}
          onClick={(e) => e.stopPropagation()}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              void save();
            } else if (e.key === "Escape") {
              onEditCancel();
            }
          }}
          onBlur={() => void save()}
          aria-label="Edit note"
        />
      ) : (
        <div className="card-content">{renderInline(item.content)}</div>
      )}
    </div>
  );
}

export default memo(ItemCard, (previous, next) => {
  return (
    previous.item.id === next.item.id &&
    previous.item.content === next.item.content &&
    previous.item.done === next.item.done &&
    previous.item.sectionId === next.item.sectionId &&
    previous.selected === next.selected &&
    previous.editing === next.editing &&
    previous.onClick === next.onClick &&
    previous.onContextMenu === next.onContextMenu &&
    previous.onToggleDone === next.onToggleDone &&
    previous.onEditSave === next.onEditSave &&
    previous.onEditCancel === next.onEditCancel &&
    previous.onDoubleClick === next.onDoubleClick
  );
});
