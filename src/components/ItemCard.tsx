import { memo, useEffect, useMemo, useRef, useState } from "react";
import { renderInline } from "../markdown";
import { api } from "../store";
import type { Item } from "../types";
import AttachmentTile, { makeDragIcon } from "./AttachmentTile";

interface Props {
  item: Item;
  selected: boolean;
  editing: boolean;
  expanded: boolean;
  settling: boolean;
  settledContext?: string;
  onClick: (e: React.MouseEvent, id: number) => void;
  onContextMenu: (e: React.MouseEvent, id: number) => void;
  onToggleDone: (id: number) => void;
  onEditSave: (id: number, content: string) => void | Promise<void>;
  onEditCancel: () => void;
  onDoubleClick: (id: number) => void;
  onOpenAttachment: (id: number) => void;
  selectedAttachmentIds: ReadonlySet<number>;
  getSelectedAttachmentPaths: () => string[];
  onSelectAttachment: (event: React.MouseEvent, id: number) => void;
}

function ItemCard({
  item,
  selected,
  editing,
  expanded,
  settling,
  settledContext,
  onClick,
  onContextMenu,
  onToggleDone,
  onEditSave,
  onEditCancel,
  onDoubleClick,
  onOpenAttachment,
  selectedAttachmentIds,
  getSelectedAttachmentPaths,
  onSelectAttachment,
}: Props) {
  const [draft, setDraft] = useState(item.content);
  const [draggingPrompt, setDraggingPrompt] = useState(false);
  const [draggingAttachments, setDraggingAttachments] = useState(false);
  const taRef = useRef<HTMLTextAreaElement>(null);
  const savingRef = useRef(false);
  const settledPreview = useMemo(() => {
    const compact = item.content.replace(/\s+/g, " ").trim();
    const characters = Array.from(compact);
    return characters.length > 120 ? `${characters.slice(0, 119).join("")}…` : compact;
  }, [item.content]);

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
  if (expanded) classes.push("expanded");
  if (settling) classes.push("settling");
  if (draggingPrompt) classes.push("dragging-prompt");
  if (draggingAttachments) classes.push("dragging-files");

  return (
    <div
      className={classes.join(" ")}
      data-item-id={item.id}
      role="option"
      aria-selected={selected}
      aria-label={`${item.done ? "Settled" : "Open"} prompt: ${item.content}`}
      draggable={!editing}
      onDragStart={(event) => {
        if (editing || (event.target as HTMLElement).closest(".attachment-tile")) return;
        event.dataTransfer.effectAllowed = "copy";
        event.dataTransfer.setData("text/plain", item.content);
        event.stopPropagation();
        event.preventDefault();
        setDraggingPrompt(true);
        void api
          .startTextDrag(item.content, makeDragIcon(item.content, 1, "prompt"))
          .finally(() => setDraggingPrompt(false));
      }}
      onDragEnd={() => {
        setDraggingPrompt(false);
        setDraggingAttachments(false);
      }}
      onClick={(event) => onClick(event, item.id)}
      onContextMenu={(event) => onContextMenu(event, item.id)}
      onDoubleClick={() => onDoubleClick(item.id)}
    >
      <button
        className={`check${item.done ? " checked" : ""}`}
        title={item.done ? "Return to active" : "Mark as settled"}
        aria-label={item.done ? "Return prompt to active" : "Mark prompt as settled"}
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
      <div className="card-body">
        {item.attachments.length > 0 && (!item.done || editing) && (
          <div className="card-attachments" aria-label={`${item.attachments.length} attachments`}>
            {item.attachments.map((attachment) => (
              <AttachmentTile
                key={attachment.id}
                attachment={attachment}
                onOpen={onOpenAttachment}
                selected={selectedAttachmentIds.has(attachment.id)}
                onSelect={onSelectAttachment}
                getDragPaths={() => {
                  const selectedPaths = getSelectedAttachmentPaths();
                  return selectedAttachmentIds.has(attachment.id) && selectedPaths.length
                    ? selectedPaths
                    : item.attachments.map((candidate) => candidate.path);
                }}
                onDragStateChange={setDraggingAttachments}
              />
            ))}
          </div>
        )}
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
        ) : item.done ? (
          <div className="card-content">{settledPreview}</div>
        ) : (
          <div className="card-content">{renderInline(item.content)}</div>
        )}
        {item.syncConflict && !editing && (
          <div className="settled-context" role="status">Concurrent edits — edit to resolve</div>
        )}
      </div>
      {item.done && settledContext && (
        <span className="settled-context">{settledContext}</span>
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
    previous.item.syncConflict === next.item.syncConflict &&
    previous.item.attachments.length === next.item.attachments.length &&
    previous.item.attachments.every(
      (attachment, index) => attachment.id === next.item.attachments[index]?.id,
    ) &&
    previous.selected === next.selected &&
    previous.editing === next.editing &&
    previous.expanded === next.expanded &&
    previous.settling === next.settling &&
    previous.settledContext === next.settledContext &&
    previous.onClick === next.onClick &&
    previous.onContextMenu === next.onContextMenu &&
    previous.onToggleDone === next.onToggleDone &&
    previous.onEditSave === next.onEditSave &&
    previous.onEditCancel === next.onEditCancel &&
    previous.onDoubleClick === next.onDoubleClick &&
    previous.onOpenAttachment === next.onOpenAttachment &&
    previous.onSelectAttachment === next.onSelectAttachment &&
    previous.getSelectedAttachmentPaths === next.getSelectedAttachmentPaths &&
    previous.item.attachments.every(
      (attachment) =>
        previous.selectedAttachmentIds.has(attachment.id) ===
        next.selectedAttachmentIds.has(attachment.id),
    )
  );
});
