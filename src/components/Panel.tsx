import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import {
  useCallback,
  useDeferredValue,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { api, useAppState } from "../store";
import type { AttachmentDraft, Item, Section, Theme } from "../types";
import AttachmentTile from "./AttachmentTile";
import ConfirmDialog from "./ConfirmDialog";
import ContextMenu, { MenuEntry } from "./ContextMenu";
import ItemCard from "./ItemCard";
import SectionSwitcher from "./SectionSwitcher";
import SettingsSheet from "./SettingsSheet";
import ShortcutsSheet from "./ShortcutsSheet";

const IS_MAC = navigator.userAgent.includes("Mac");
const MOD = IS_MAC ? "⌘" : "Ctrl";
const DELETE_KEY = IS_MAC ? "⌫" : "Del";
const THEMES: { value: Theme; label: string }[] = [
  { value: "system", label: "System appearance" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
  { value: "glass", label: "Glass" },
];

interface Group {
  section: Section | null;
  items: Item[];
}

interface ClearSettledRequest {
  ids: number[];
  scope: string;
}

function isListCommand(value: string) {
  return value.startsWith("## ") || value.startsWith("# ");
}

export default function Panel() {
  const { state, error } = useAppState();
  const [query, setQuery] = useState("");
  const [input, setInput] = useState("");
  const [attachments, setAttachments] = useState<AttachmentDraft[]>([]);
  const [draggingFiles, setDraggingFiles] = useState(false);
  const [draggingText, setDraggingText] = useState(false);
  const [attaching, setAttaching] = useState(false);
  const [showAllLists, setShowAllLists] = useState(true);
  const [selected, setSelected] = useState<number[]>([]);
  const [selectedAttachments, setSelectedAttachments] = useState<number[]>([]);
  const [expanded, setExpanded] = useState<number[]>([]);
  const [movingIds, setMovingIds] = useState<number[] | null>(null);
  const [anchor, setAnchor] = useState<number | null>(null);
  const [editingId, setEditingId] = useState<number | null>(null);
  const [itemMenu, setItemMenu] = useState<{ x: number; y: number; ids: number[] } | null>(null);
  const [sectionMenu, setSectionMenu] = useState<{ x: number; y: number; id: number } | null>(null);
  const [renamingSection, setRenamingSection] = useState<number | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [showSwitcher, setShowSwitcher] = useState(false);
  const [showHelp, setShowHelp] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [appMenu, setAppMenu] = useState<{ x: number; y: number } | null>(null);
  const [toast, setToast] = useState<{ message: string; error: boolean } | null>(null);
  const [toastExiting, setToastExiting] = useState(false);
  const [confirmClear, setConfirmClear] = useState<ClearSettledRequest | null>(null);
  const [deletingList, setDeletingList] = useState<Section | null>(null);
  const [settlingIds, setSettlingIds] = useState<number[]>([]);
  const [attachmentAnchor, setAttachmentAnchor] = useState<number | null>(null);
  const [accessibilityGranted, setAccessibilityGranted] = useState<boolean | null>(null);
  // Bumped on every panel-shown; drives the reveal animation on the panel root.
  const [revealTick, setRevealTick] = useState(0);

  const inputRef = useRef<HTMLTextAreaElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const composerRef = useRef<HTMLDivElement>(null);
  const attachmentsRef = useRef<AttachmentDraft[]>(attachments);
  const toastTimer = useRef<number>();
  const toastExitTimer = useRef<number>();
  const settleTimers = useRef<number[]>([]);
  const settlingIdsRef = useRef(new Set<number>());
  const dragKindRef = useRef<"files" | "text" | "none">("none");
  const scrollAfterAdd = useRef(false);
  const deferredQuery = useDeferredValue(query);
  attachmentsRef.current = attachments;

  const flashToast = useCallback((msg: string, error = false) => {
    setToast({ message: msg, error });
    setToastExiting(false);
    window.clearTimeout(toastTimer.current);
    window.clearTimeout(toastExitTimer.current);
    toastExitTimer.current = window.setTimeout(() => setToastExiting(true), 2050);
    toastTimer.current = window.setTimeout(() => setToast(null), 2200);
  }, []);

  useEffect(
    () => () => {
      window.clearTimeout(toastTimer.current);
      window.clearTimeout(toastExitTimer.current);
      settleTimers.current.forEach((timer) => window.clearTimeout(timer));
    },
    [],
  );

  const reportError = useCallback(
    (action: string, cause: unknown) => {
      const detail = cause instanceof Error ? cause.message : String(cause);
      flashToast(`${action}: ${detail}`, true);
    },
    [flashToast],
  );

  const addAttachmentPaths = useCallback(
    async (paths: string[]) => {
      const existing = new Set(attachmentsRef.current.map((attachment) => attachment.path));
      const unique = paths.filter((path) => !existing.has(path));
      const available = 12 - attachmentsRef.current.length;
      if (available <= 0) {
        flashToast("A prompt can have up to 12 files");
        return;
      }
      const accepted = unique.slice(0, available);
      if (!accepted.length) return;
      setAttaching(true);
      try {
        const inspected = await api.inspectAttachments(accepted);
        setAttachments((current) => {
          const currentPaths = new Set(current.map((attachment) => attachment.path));
          return [
            ...current,
            ...inspected.filter((attachment) => !currentPaths.has(attachment.path)),
          ].slice(0, 12);
        });
        const count = inspected.length;
        if (count) flashToast(`Attached ${count} ${count === 1 ? "file" : "files"}`);
        if (unique.length > available) flashToast("Attached the first 12 files");
        requestAnimationFrame(() => inputRef.current?.focus());
      } catch (cause) {
        reportError("Couldn’t attach file", cause);
      } finally {
        setAttaching(false);
      }
    },
    [flashToast, reportError],
  );

  const pickAttachments = useCallback(async () => {
    try {
      const selected = await open({
        multiple: true,
        directory: false,
        title: "Attach files to this prompt",
      });
      if (!selected) return;
      await addAttachmentPaths(Array.isArray(selected) ? selected : [selected]);
    } catch (cause) {
      reportError("Couldn’t open files", cause);
    }
  }, [addAttachmentPaths, reportError]);

  const pasteClipboardImage = useCallback(async () => {
    if (attachmentsRef.current.length >= 12) {
      flashToast("A prompt can have up to 12 files");
      return;
    }
    setAttaching(true);
    try {
      const attachment = await api.pasteClipboardImage();
      setAttachments((current) => [...current, attachment].slice(0, 12));
      flashToast("Pasted image");
    } catch (cause) {
      reportError("Couldn’t paste image", cause);
    } finally {
      setAttaching(false);
    }
  }, [flashToast, reportError]);

  const removeAttachment = useCallback(
    (attachment: AttachmentDraft) => {
      setAttachments((current) =>
        current.filter((candidate) => candidate.path !== attachment.path),
      );
      if (attachment.temporary) {
        void api
          .discardPastedImage(attachment.path)
          .catch((cause) => reportError("Couldn’t discard pasted image", cause));
      }
    },
    [reportError],
  );

  const appendDroppedText = useCallback(
    (value: string) => {
      const droppedText = value.trim();
      if (!droppedText) return;
      dragKindRef.current = "none";
      setDraggingFiles(false);
      setDraggingText(false);
      setInput((current) =>
        current.trim() ? `${current.trimEnd()}\n\n${droppedText}` : droppedText,
      );
      requestAnimationFrame(() => {
        const textarea = inputRef.current;
        if (!textarea) return;
        textarea.style.height = "auto";
        textarea.style.height = `${Math.min(textarea.scrollHeight, 120)}px`;
        textarea.focus();
        textarea.setSelectionRange(textarea.value.length, textarea.value.length);
      });
      flashToast("Added dropped text");
    },
    [flashToast],
  );

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void getCurrentWebview()
      .onDragDropEvent(({ payload }) => {
        if (payload.type === "over") {
          const text = dragKindRef.current === "text";
          setDraggingText(text);
          setDraggingFiles(!text);
        } else if (payload.type === "drop") {
          dragKindRef.current = "none";
          setDraggingFiles(false);
          setDraggingText(false);
          if (payload.paths.length) void addAttachmentPaths(payload.paths);
        } else {
          dragKindRef.current = "none";
          setDraggingFiles(false);
          setDraggingText(false);
        }
      })
      .then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      })
      .catch((cause) => reportError("Couldn’t start file drop", cause));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [addAttachmentPaths, reportError]);

  const refreshAccessibility = useCallback(() => {
    if (!IS_MAC) return;
    void api
      .accessibilityStatus()
      .then(setAccessibilityGranted)
      .catch((cause) => reportError("Couldn’t check Accessibility", cause));
  }, [reportError]);

  const requestAccessibility = useCallback(async () => {
    try {
      const granted = await api.requestAccessibilityPermission();
      setAccessibilityGranted(granted);
      if (!granted) {
        flashToast("Finish setup in System Settings, then return to Clippy");
        await api.openAccessibilitySettings();
      }
    } catch (cause) {
      reportError("Couldn’t request Accessibility", cause);
    }
  }, [flashToast, reportError]);

  // Focus the quick-add input whenever the panel is summoned; brief toast on capture.
  useEffect(() => {
    let disposed = false;
    const unlisteners: (() => void)[] = [];
    const addListener = <T,>(event: string, handler: (payload: T) => void) => {
      void listen<T>(event, ({ payload }) => handler(payload))
        .then((stop) => {
          if (disposed) stop();
          else unlisteners.push(stop);
        })
        .catch((cause) => reportError("Couldn’t start app events", cause));
    };
    addListener("panel-shown", () => {
      setRevealTick((tick) => tick + 1);
      inputRef.current?.focus();
      refreshAccessibility();
    });
    addListener("captured", () => flashToast("Captured"));
    addListener("capture-empty", () => flashToast("No text selected"));
    addListener("capture-duplicate", () => flashToast("Already captured"));
    addListener<"files" | "text" | "none">("native-drag-kind", (kind) => {
      dragKindRef.current = kind;
      setDraggingText(kind === "text");
      setDraggingFiles(kind === "files");
    });
    addListener<string>("text-dropped", appendDroppedText);
    addListener<string>("capture-error", (message) => {
      flashToast(message);
      refreshAccessibility();
    });
    refreshAccessibility();
    window.addEventListener("focus", refreshAccessibility);
    return () => {
      disposed = true;
      window.removeEventListener("focus", refreshAccessibility);
      unlisteners.forEach((stop) => stop());
    };
  }, [appendDroppedText, flashToast, refreshAccessibility, reportError]);

  const sections = state?.sections ?? [];
  const items = state?.items ?? [];
  const activeSectionId = state?.activeSectionId ?? null;
  const activeSection = sections.find((s) => s.id === activeSectionId) ?? null;

  const groups = useMemo<Group[]>(() => {
    const q = deferredQuery.trim().toLocaleLowerCase();
    const buckets = new Map<number | null, Item[]>();
    for (const item of items) {
      if (
        q &&
        !item.content.toLocaleLowerCase().includes(q) &&
        !item.attachments.some((attachment) => attachment.name.toLocaleLowerCase().includes(q))
      ) {
        continue;
      }
      const bucket = buckets.get(item.sectionId);
      if (bucket) bucket.push(item);
      else buckets.set(item.sectionId, [item]);
    }
    const result: Group[] = [];
    const activeFirst = (bucket: Item[]) =>
      bucket.slice().sort((left, right) => Number(left.done) - Number(right.done));
    const unfiled = activeFirst(buckets.get(null) ?? []);
    if (showAllLists && unfiled.length) result.push({ section: null, items: unfiled });
    if (!showAllLists && activeSectionId === null) {
      result.push({ section: null, items: unfiled });
      return result;
    }
    for (const s of sections) {
      const inSection = activeFirst(buckets.get(s.id) ?? []);
      if (!showAllLists && s.id !== activeSectionId) continue;
      // Show an empty section header only when it's the active target (and not searching).
      if (inSection.length || (!q && s.id === activeSectionId)) {
        result.push({ section: s, items: inSection });
      }
    }
    return result;
  }, [items, sections, deferredQuery, activeSectionId, showAllLists]);

  const activeGroups = useMemo(
    () =>
      groups
        .map((group) => ({
          ...group,
          items: group.items.filter((item) => !item.done),
        }))
        .filter(
          (group) =>
            group.items.length > 0 ||
            (!showAllLists && !deferredQuery.trim() && group.section?.id === activeSectionId),
        ),
    [activeSectionId, deferredQuery, groups, showAllLists],
  );
  const settledItems = useMemo(
    () => groups.flatMap((group) => group.items.filter((item) => item.done)),
    [groups],
  );
  const flatIds = useMemo(
    () => [
      ...activeGroups.flatMap((group) => group.items.map((item) => item.id)),
      ...settledItems.map((item) => item.id),
    ],
    [activeGroups, settledItems],
  );
  const flatAttachmentIds = useMemo(
    () =>
      activeGroups.flatMap((group) =>
        group.items.flatMap((item) => item.attachments.map((file) => file.id)),
      ),
    [activeGroups],
  );
  const itemById = useMemo(() => new Map(items.map((i) => [i.id, i])), [items]);
  const sectionNameById = useMemo(
    () => new Map(sections.map((section) => [section.id, section.name])),
    [sections],
  );
  const attachmentById = useMemo(
    () => new Map(items.flatMap((item) => item.attachments.map((file) => [file.id, file] as const))),
    [items],
  );
  const selectedSet = useMemo(() => new Set(selected), [selected]);
  const settlingSet = useMemo(() => new Set(settlingIds), [settlingIds]);
  const selectedAttachmentSet = useMemo(
    () => new Set(selectedAttachments),
    [selectedAttachments],
  );
  const selectedAttachmentPaths = useMemo(
    () => selectedAttachments.flatMap((id) => attachmentById.get(id)?.path ?? []),
    [attachmentById, selectedAttachments],
  );
  const selectedAttachmentPathsRef = useRef(selectedAttachmentPaths);
  selectedAttachmentPathsRef.current = selectedAttachmentPaths;
  const getSelectedAttachmentPaths = useCallback(
    () => selectedAttachmentPathsRef.current,
    [],
  );
  const expandedSet = useMemo(() => new Set(expanded), [expanded]);
  const currentScope = showAllLists ? "All" : (activeSection?.name ?? "Inbox");
  const settledIdsInScope = useMemo(
    () =>
      items
        .filter(
          (item) => item.done && (showAllLists || item.sectionId === activeSectionId),
        )
        .map((item) => item.id),
    [activeSectionId, items, showAllLists],
  );
  const orderById = useMemo(() => new Map(flatIds.map((id, index) => [id, index])), [flatIds]);
  const flatIdsRef = useRef(flatIds);
  const anchorRef = useRef(anchor);
  const selectedRef = useRef(selected);
  const selectedSetRef = useRef(selectedSet);
  const itemByIdRef = useRef(itemById);
  flatIdsRef.current = flatIds;
  anchorRef.current = anchor;
  selectedRef.current = selected;
  selectedSetRef.current = selectedSet;
  itemByIdRef.current = itemById;

  useEffect(() => {
    setSelected((current) => current.filter((id) => itemById.has(id)));
    setSelectedAttachments((current) => current.filter((id) => attachmentById.has(id)));
    setExpanded((current) => current.filter((id) => itemById.has(id)));
  }, [attachmentById, itemById]);

  useEffect(() => {
    if (!scrollAfterAdd.current) return;
    scrollAfterAdd.current = false;
    requestAnimationFrame(() => {
      const activeCards = listRef.current?.querySelectorAll<HTMLElement>(".card:not(.done)");
      activeCards?.[activeCards.length - 1]?.scrollIntoView({ block: "nearest" });
    });
  }, [items.length]);

  const copySelection = useCallback(
    (asList: boolean, ids?: number[], attachmentIds?: number[]) => {
      const targets = (ids ?? selected)
        .slice()
        .sort((a, b) => (orderById.get(a) ?? 0) - (orderById.get(b) ?? 0))
        .map((id) => itemById.get(id))
        .filter((i): i is Item => !!i);
      const chosenAttachmentIds = attachmentIds ?? (ids ? [] : selectedAttachments);
      const chosenAttachments = chosenAttachmentIds
        .map((id) => attachmentById.get(id))
        .filter((attachment) => !!attachment);
      if (!targets.length && !chosenAttachments.length) return;
      const text = targets.length
        ? asList
          ? targets
            .map((item, index) => `${index + 1}. ${item.content.replace(/\n/g, "\n   ")}`)
            .join("\n")
          : targets.map((item) => item.content).join("\n\n")
        : chosenAttachments.map((attachment) => attachment.name).join("\n");
      const paths = Array.from(
        new Set([
          ...targets.flatMap((item) => item.attachments.map((attachment) => attachment.path)),
          ...chosenAttachments.map((attachment) => attachment.path),
        ]),
      );
      void api
        .copyText(text, paths)
        .then(() =>
          flashToast(
            paths.length
              ? `Copied ${paths.length} ${paths.length === 1 ? "file" : "files"}`
              : asList
                ? "Copied as list"
                : "Copied",
          ),
        )
        .catch((cause) => reportError("Couldn’t copy", cause));
    },
    [
      selected,
      selectedAttachments,
      orderById,
      itemById,
      attachmentById,
      flashToast,
      reportError,
    ],
  );

  const deleteIds = useCallback(
    async (ids: number[]) => {
      try {
        await api.deleteItems(ids);
        setSelected([]);
        setSelectedAttachments([]);
      } catch (cause) {
        reportError("Couldn’t delete", cause);
      }
    },
    [reportError],
  );

  const mergeIds = useCallback(
    async (ids: number[]) => {
      const ordered = ids
        .slice()
        .sort((left, right) => (orderById.get(left) ?? 0) - (orderById.get(right) ?? 0));
      if (ordered.length < 2) return;
      try {
        await api.mergeItems(ordered);
        setSelected([ordered[0]]);
        setAnchor(ordered[0]);
        flashToast(`Merged ${ordered.length} prompts`);
      } catch (cause) {
        reportError("Couldn’t merge prompts", cause);
      }
    },
    [flashToast, orderById, reportError],
  );

  const setIdsDone = useCallback(
    (ids: number[]) => {
      const targets = ids.map((id) => itemById.get(id)).filter((item): item is Item => !!item);
      const done = !targets.every((item) => item.done);
      if (!done) {
        ids.forEach((id) => settlingIdsRef.current.delete(id));
        setSettlingIds((current) => current.filter((id) => !ids.includes(id)));
        void api.setItemsDone(ids, false).catch((cause) => reportError("Couldn’t update", cause));
        return;
      }

      const newlySettling = targets
        .filter((item) => !item.done && !settlingIdsRef.current.has(item.id))
        .map((item) => item.id);
      if (!newlySettling.length) return;
      newlySettling.forEach((id) => settlingIdsRef.current.add(id));
      const hiddenAttachmentIds = new Set(
        targets.flatMap((item) => item.attachments.map((attachment) => attachment.id)),
      );
      setExpanded((current) => current.filter((id) => !newlySettling.includes(id)));
      setSelectedAttachments((current) =>
        current.filter((id) => !hiddenAttachmentIds.has(id)),
      );
      setSettlingIds((current) => Array.from(new Set([...current, ...newlySettling])));

      const commitTimer = window.setTimeout(() => {
        settleTimers.current = settleTimers.current.filter((timer) => timer !== commitTimer);
        void api
          .setItemsDone(newlySettling, true)
          .then(() => {
            const finishTimer = window.setTimeout(() => {
              settleTimers.current = settleTimers.current.filter(
                (timer) => timer !== finishTimer,
              );
              setSettlingIds((current) =>
                current.filter((id) => !newlySettling.includes(id)),
              );
              newlySettling.forEach((id) => settlingIdsRef.current.delete(id));
            }, 260);
            settleTimers.current.push(finishTimer);
          })
          .catch((cause) => {
            setSettlingIds((current) =>
              current.filter((id) => !newlySettling.includes(id)),
            );
            newlySettling.forEach((id) => settlingIdsRef.current.delete(id));
            reportError("Couldn’t update", cause);
          });
      }, 145);
      settleTimers.current.push(commitTimer);
    },
    [itemById, reportError],
  );

  const requestClearSettled = useCallback((scope: string, ids: number[]) => {
    if (ids.length) setConfirmClear({ scope, ids });
  }, []);

  const settledIdsForList = useCallback(
    (sectionId: number | null) =>
      items
        .filter((item) => item.done && item.sectionId === sectionId)
        .map((item) => item.id),
    [items],
  );
  const setIdsDoneRef = useRef(setIdsDone);
  setIdsDoneRef.current = setIdsDone;

  const onItemClick = useCallback((e: React.MouseEvent, id: number) => {
    if (e.ctrlKey || e.metaKey) {
      setSelected((sel) => (sel.includes(id) ? sel.filter((x) => x !== id) : [...sel, id]));
      setAnchor(id);
    } else if (e.shiftKey && anchorRef.current !== null) {
      const a = flatIdsRef.current.indexOf(anchorRef.current);
      const b = flatIdsRef.current.indexOf(id);
      if (a !== -1 && b !== -1) {
        setSelected(flatIdsRef.current.slice(Math.min(a, b), Math.max(a, b) + 1));
      }
    } else {
      setSelected([id]);
      setSelectedAttachments([]);
      setAnchor(id);
    }
  }, []);

  const onSelectAttachment = useCallback(
    (event: React.MouseEvent, id: number) => {
      event.stopPropagation();
      if (event.ctrlKey || event.metaKey) {
        setSelectedAttachments((current) =>
          current.includes(id) ? current.filter((value) => value !== id) : [...current, id],
        );
      } else if (event.shiftKey && attachmentAnchor !== null) {
        const first = flatAttachmentIds.indexOf(attachmentAnchor);
        const last = flatAttachmentIds.indexOf(id);
        if (first !== -1 && last !== -1) {
          setSelectedAttachments(
            flatAttachmentIds.slice(Math.min(first, last), Math.max(first, last) + 1),
          );
        }
      } else {
        setSelected([]);
        setSelectedAttachments([id]);
        setAnchor(null);
      }
      setAttachmentAnchor(id);
    },
    [attachmentAnchor, flatAttachmentIds],
  );

  const onItemContextMenu = useCallback((e: React.MouseEvent, id: number) => {
    e.preventDefault();
    const ids = selectedSetRef.current.has(id) ? selectedRef.current : [id];
    if (!selectedSetRef.current.has(id)) {
      setSelected([id]);
      setSelectedAttachments([]);
      setAnchor(id);
    }
    setItemMenu({ x: e.clientX, y: e.clientY, ids });
  }, []);

  const onToggleItemDone = useCallback((id: number) => {
    setIdsDoneRef.current([id]);
  }, []);

  const onOpenAttachment = useCallback(
    (id: number) => {
      void api
        .openAttachment(id)
        .catch((cause) => reportError("Couldn’t open attachment", cause));
    },
    [reportError],
  );

  const onEditItem = useCallback((id: number) => setEditingId(id), []);
  const onCancelItemEdit = useCallback(() => setEditingId(null), []);
  const onSaveItemEdit = useCallback(
    async (id: number, content: string) => {
      if (!content.trim()) {
        flashToast("A note cannot be empty");
        return;
      }
      const item = itemByIdRef.current.get(id);
      if (!item) {
        reportError("Couldn’t save", "That note no longer exists");
        return;
      }
      if (content !== item.content) {
        try {
          await api.updateItem(id, content);
        } catch (cause) {
          reportError("Couldn’t save", cause);
          return;
        }
      }
      setEditingId(null);
    },
    [flashToast, reportError],
  );

  // Global keyboard handling.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement;
      const inField = target.tagName === "TEXTAREA" || target.tagName === "INPUT";
      const mod = e.ctrlKey || e.metaKey;
      const slashKey =
        e.key === "/" ||
        e.key === "?" ||
        e.code === "Slash" ||
        e.code === "IntlRo";

      // `event.key` changes with the active keyboard layout. Brazilian Pro,
      // for example, can expose the physical slash key as IntlRo. Handle the
      // semantic and physical variants, and let the shortcut toggle the sheet.
      if (mod && slashKey) {
        e.preventDefault();
        setShowHelp((v) => !v);
        return;
      }

      // Modal surfaces own their keyboard handling. Keeping list shortcuts out
      // prevents an arrow key in a menu from moving the selection behind it.
      if (
        confirmClear ||
        deletingList ||
        showHelp ||
        showSettings ||
        showSwitcher ||
        itemMenu ||
        sectionMenu ||
        appMenu
      ) return;

      if (mod && e.key.toLowerCase() === "f") {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
        return;
      }
      if (mod && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setMovingIds(null);
        setShowSwitcher((v) => !v);
        return;
      }
      if (e.key === "Escape") {
        if (editingId !== null) return;
        if (inField && (target as HTMLTextAreaElement).value) return;
        if (selected.length || selectedAttachments.length) {
          setSelected([]);
          setSelectedAttachments([]);
          return;
        }
        void api.hidePanel().catch((cause) => reportError("Couldn’t hide Clippy", cause));
        return;
      }
      if (mod && e.key.toLowerCase() === "w") {
        e.preventDefault();
        void api.hidePanel().catch((cause) => reportError("Couldn’t hide Clippy", cause));
        return;
      }
      if (inField || editingId !== null || showSwitcher || showHelp || showSettings) return;

      if (mod && e.key.toLowerCase() === "a") {
        e.preventDefault();
        setSelected(flatIds);
        setSelectedAttachments(flatAttachmentIds);
        if (flatIds.length) setAnchor(flatIds[flatIds.length - 1] ?? null);
        return;
      }

      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        if (!flatIds.length) return;
        const current = anchor !== null ? flatIds.indexOf(anchor) : -1;
        const next =
          e.key === "ArrowDown"
            ? Math.min(current + 1, flatIds.length - 1)
            : Math.max(current === -1 ? flatIds.length - 1 : current - 1, 0);
        const id = flatIds[next];
        setSelected([id]);
        setSelectedAttachments([]);
        setAnchor(id);
        document
          .querySelector(`[data-item-id="${id}"]`)
          ?.scrollIntoView({ block: "nearest" });
      } else if (e.key === " " && selected.length) {
        e.preventDefault();
        setIdsDone(selected);
      } else if (e.key === "Enter" && selected.length === 1) {
        e.preventDefault();
        if (mod) {
          void api
            .openEditor(selected[0])
            .catch((cause) => reportError("Couldn’t open editor", cause));
        } else setEditingId(selected[0]);
      } else if ((e.key === "Delete" || e.key === "Backspace") && selected.length) {
        e.preventDefault();
        deleteIds(selected);
      } else if (
        mod &&
        !e.shiftKey &&
        e.key.toLowerCase() === "c" &&
        (selected.length || selectedAttachments.length)
      ) {
        if (window.getSelection()?.toString()) return;
        e.preventDefault();
        copySelection(false);
      } else if (
        mod &&
        e.shiftKey &&
        e.key.toLowerCase() === "c" &&
        (selected.length || selectedAttachments.length)
      ) {
        e.preventDefault();
        copySelection(true);
      } else if (mod && e.shiftKey && e.key.toLowerCase() === "m" && selected.length > 1) {
        e.preventDefault();
        void mergeIds(selected);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [
    flatIds,
    flatAttachmentIds,
    anchor,
    selected,
    selectedAttachments,
    editingId,
    showSwitcher,
    showHelp,
    showSettings,
    itemMenu,
    sectionMenu,
    appMenu,
    confirmClear,
    deletingList,
    copySelection,
    deleteIds,
    mergeIds,
    setIdsDone,
    reportError,
  ]);

  const submitInput = async () => {
    const text = input.trim();
    if (!text) return;
    try {
      const listCommand = isListCommand(text);
      scrollAfterAdd.current = !listCommand;
      await api.addEntry(
        text,
        attachments.map((attachment) => attachment.path),
      );
      setInput("");
      setAttachments([]);
      if (inputRef.current) inputRef.current.style.height = "auto";
      if (listCommand) {
        const name = text.startsWith("## ") ? text.slice(3).trim() : text.slice(2).trim();
        flashToast(`List “${name}”`);
        setShowAllLists(false);
      }
    } catch (cause) {
      scrollAfterAdd.current = false;
      reportError("Couldn’t add note", cause);
    }
  };

  const menuEntries = (ids: number[]): MenuEntry[] => {
    const targets = ids.map((id) => itemById.get(id)).filter((i): i is Item => !!i);
    const allDone = targets.every((i) => i.done);
    const single = targets.length === 1;
    return [
      { label: "Copy", kbd: `${MOD} C`, onClick: () => copySelection(false, ids) },
      { label: "Copy as List", kbd: `${MOD} ⇧ C`, onClick: () => copySelection(true, ids) },
      {
        label: allDone ? "Return to Active" : "Mark as Settled",
        kbd: "Space",
        separatorBefore: true,
        onClick: () => setIdsDone(ids),
      },
      {
        label: single && expandedSet.has(ids[0]) ? "Collapse" : "Expand",
        disabled: !single || targets[0]?.done,
        onClick: () => {
          if (!single) return;
          setExpanded((current) =>
            current.includes(ids[0])
              ? current.filter((id) => id !== ids[0])
              : [...current, ids[0]],
          );
        },
      },
      {
        label: "Edit",
        kbd: "↵",
        disabled: !single,
        onClick: () => single && setEditingId(ids[0]),
      },
      {
        label: "Edit in New Window",
        kbd: `${MOD} ↵`,
        disabled: !single,
        onClick: () => {
          if (single) {
            void api
              .openEditor(ids[0])
              .catch((cause) => reportError("Couldn’t open editor", cause));
          }
        },
      },
      {
        label: "Merge Notes",
        kbd: `${MOD} ⇧ M`,
        disabled: targets.length < 2,
        onClick: () => void mergeIds(ids),
      },
      {
        label: targets.length > 1 ? `Move ${targets.length} prompts…` : "Move to list…",
        separatorBefore: true,
        onClick: () => {
          setMovingIds(ids);
          setShowSwitcher(true);
        },
      },
      {
        label: targets.length > 1 ? `Delete ${targets.length} items` : "Delete",
        kbd: DELETE_KEY,
        danger: true,
        separatorBefore: true,
        onClick: () => deleteIds(ids),
      },
    ];
  };

  const sectionMenuEntries = (id: number): MenuEntry[] => [
    {
      label: "Capture here",
      onClick: () =>
        void api
          .setActiveSection(id)
          .catch((cause) => reportError("Couldn’t switch list", cause)),
    },
    {
      label: "Rename",
      onClick: () => {
        const s = sections.find((x) => x.id === id);
        setRenameDraft(s?.name ?? "");
        setRenamingSection(id);
      },
    },
    {
      label: "Move all prompts to Inbox",
      disabled: !items.some((item) => item.sectionId === id),
      separatorBefore: true,
      onClick: () => {
        const ids = items.filter((item) => item.sectionId === id).map((item) => item.id);
        if (!ids.length) return;
        void api
          .moveItems(ids, null)
          .then(() => flashToast(`Moved ${ids.length} ${ids.length === 1 ? "prompt" : "prompts"} to Inbox`))
          .catch((cause) => reportError("Couldn’t move prompts", cause));
      },
    },
    {
      label: "Clear settled prompts…",
      disabled: settledIdsForList(id).length === 0,
      onClick: () => {
        const section = sections.find((candidate) => candidate.id === id);
        requestClearSettled(section?.name ?? "this list", settledIdsForList(id));
      },
    },
    {
      label: "Delete list · keep prompts in Inbox",
      danger: true,
      onClick: () =>
        void api
          .deleteSection(id, false)
          .then(() => flashToast("List removed · prompts moved to Inbox"))
          .catch((cause) => reportError("Couldn’t remove list", cause)),
    },
    {
      label: "Delete list and prompts…",
      danger: true,
      onClick: () => {
        const section = sections.find((candidate) => candidate.id === id);
        if (section) setDeletingList(section);
      },
    },
  ];

  const appMenuEntries = (): MenuEntry[] => {
    const theme = state?.theme ?? "system";
    const entries: MenuEntry[] = [
      {
        label: "Show all",
        checked: showAllLists,
        onClick: () => setShowAllLists(true),
      },
      {
        label: "Inbox",
        checked: !showAllLists && activeSectionId === null,
        onClick: () => {
          setShowAllLists(false);
          void api
            .setActiveSection(null)
            .catch((cause) => reportError("Couldn’t switch list", cause));
        },
      },
      ...sections.map((section) => ({
        label: section.name,
        checked: !showAllLists && section.id === activeSectionId,
        onClick: () => {
          setShowAllLists(false);
          void api
            .setActiveSection(section.id)
            .catch((cause) => reportError("Couldn’t switch list", cause));
        },
      })),
      {
        label: "New list…",
        kbd: `${MOD} K`,
        separatorBefore: true,
        onClick: () => {
          setMovingIds(null);
          setShowSwitcher(true);
        },
      },
      {
        label: "Capture clipboard",
        onClick: () => void api.captureNow().catch((cause) => reportError("Couldn’t capture clipboard", cause)),
      },
      ...THEMES.map((option, index) => ({
        label: option.label,
        checked: option.value === theme,
        separatorBefore: index === 0,
        onClick: () =>
          void api.setTheme(option.value).catch((cause) => reportError("Couldn’t change appearance", cause)),
      })),
      {
        label: "Keep on Top",
        checked: state?.keepOnTop ?? true,
        separatorBefore: true,
        onClick: () =>
          void api
            .setKeepOnTop(!(state?.keepOnTop ?? true))
            .catch((cause) => reportError("Couldn’t change window behavior", cause)),
      },
      {
        label: "Export to Markdown",
        separatorBefore: true,
        onClick: async () => {
          try {
            const path = await api.exportMarkdown();
            flashToast(`Exported → ${path}`);
          } catch (cause) {
            reportError("Couldn’t export", cause);
          }
        },
      },
      {
        label: IS_MAC ? "Reveal Notes in Finder" : "Reveal local notes",
        onClick: () =>
          void api.revealNotes().catch((cause) => reportError("Couldn’t reveal notes", cause)),
      },
      {
        label:
          currentScope === "All"
            ? "Clear all settled…"
            : `Clear settled in ${currentScope}…`,
        danger: true,
        disabled: settledIdsInScope.length === 0,
        onClick: () => requestClearSettled(currentScope, settledIdsInScope),
      },
      { label: "Shortcuts", kbd: `${MOD} /`, separatorBefore: true, onClick: () => setShowHelp(true) },
      { label: "Settings…", onClick: () => setShowSettings(true) },
      {
        label: "Check for Updates…",
        onClick: () =>
          void api
            .checkForUpdates()
            .catch((cause) => reportError("Couldn’t check for updates", cause)),
      },
      {
        label: "Close",
        kbd: `${MOD} W`,
        separatorBefore: true,
        onClick: () =>
          void api.hidePanel().catch((cause) => reportError("Couldn’t hide Clippy", cause)),
      },
    ];
    if (IS_MAC) {
      entries.push({
        label: accessibilityGranted ? "Capture permissions: Allowed" : "Set Up Capture Permissions…",
        disabled: accessibilityGranted === true,
        onClick: () => void requestAccessibility(),
      });
    }
    return entries;
  };

  const isEmpty = items.length === 0;

  return (
    <div
      className="panel"
      data-reveal={revealTick ? (revealTick % 2 ? "odd" : "even") : undefined}
      aria-busy={!state}
    >
      <div className="topbar" data-tauri-drag-region>
        <div className="search">
          <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden>
            <circle cx="7" cy="7" r="4.5" fill="none" stroke="currentColor" strokeWidth="1.5" />
            <path d="M10.5 10.5 L14 14" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          </svg>
          <input
            ref={searchRef}
            aria-label="Search notes"
            placeholder="Search"
            spellCheck={false}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape" && query) {
                e.stopPropagation();
                setQuery("");
              }
            }}
          />
        </div>
        <button
          className="icon-btn"
          title="Clippy menu"
          aria-label="Open Clippy menu"
          aria-haspopup="menu"
          aria-expanded={!!appMenu}
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            setAppMenu({ x: r.right - 200, y: r.bottom + 6 });
          }}
        >
          <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden>
            <circle cx="3" cy="8" r="1.15" fill="currentColor" />
            <circle cx="8" cy="8" r="1.15" fill="currentColor" />
            <circle cx="13" cy="8" r="1.15" fill="currentColor" />
          </svg>
        </button>
      </div>

      {IS_MAC && accessibilityGranted === false && (
        <button
          className="permission-banner"
          aria-label="Set up Accessibility for instant capture"
          onClick={() => void requestAccessibility()}
        >
          <span className="permission-icon" aria-hidden>⌘</span>
          <span>
            <strong>Enable instant capture</strong>
            <small>Allow Accessibility and Input Monitoring for Double Shift</small>
          </span>
          <span className="permission-arrow" aria-hidden>›</span>
        </button>
      )}

      <div
        className="list"
        ref={listRef}
        role="listbox"
        aria-label="Captured notes"
        aria-multiselectable="true"
        onMouseDown={(e) => {
          if (e.target === e.currentTarget) {
            setSelected([]);
            setSelectedAttachments([]);
          }
        }}
      >
        {error ? (
          <div className="state-error" role="alert">
            <strong>Couldn’t load your notes</strong>
            <span>{error}</span>
          </div>
        ) : !state ? (
          <div className="loading" role="status" aria-label="Loading notes">
            <span />
          </div>
        ) : isEmpty && !query ? (
          <div className="empty">
            <div className="empty-title">Nothing captured yet</div>
            <div className="empty-sub">
              An answer, a link, a half-formed prompt. It all waits here.
            </div>
            <div className="empty-rows">
              <div className="empty-row">
                <span>Capture selected text</span>
                <span>
                  <kbd>Left Shift</kbd> <kbd>Left Shift</kbd>
                </span>
              </div>
              <div className="empty-row">
                <span>Show Clippy</span>
                <span>
                  <kbd>Right Shift</kbd> <kbd>Right Shift</kbd>
                </span>
              </div>
              <div className="empty-row">
                <span>Create a list</span>
                <span>
                  <kbd>## Title</kbd>
                </span>
              </div>
              <div className="empty-row">
                <span>See all shortcuts</span>
                <span>
                  <kbd>{MOD} /</kbd>
                </span>
              </div>
            </div>
          </div>
        ) : (
          activeGroups.map((g) => (
            <div key={g.section?.id ?? "unfiled"} className="group">
              {g.section && (
                <div
                  className="group-header"
                  onContextMenu={(e) => {
                    e.preventDefault();
                    setSectionMenu({ x: e.clientX, y: e.clientY, id: g.section!.id });
                  }}
                >
                  {renamingSection === g.section.id ? (
                    <input
                      className="group-rename"
                      autoFocus
                      value={renameDraft}
                      onChange={(e) => setRenameDraft(e.target.value)}
                      onKeyDown={async (e) => {
                        e.stopPropagation();
                        if (e.key === "Enter") {
                          if (!renameDraft.trim()) return;
                          try {
                            await api.renameSection(g.section!.id, renameDraft);
                            setRenamingSection(null);
                          } catch (cause) {
                            reportError("Couldn’t rename list", cause);
                          }
                        } else if (e.key === "Escape") {
                          setRenamingSection(null);
                        }
                      }}
                      onBlur={() => setRenamingSection(null)}
                    />
                  ) : (
                    <span
                      className="group-name"
                      title={
                        g.section.id === activeSectionId
                          ? "New captures land here"
                          : "Right-click for options"
                      }
                    >
                      {g.section.name}
                    </span>
                  )}
                  <span className="group-rule" />
                  {renamingSection !== g.section.id && (
                    <button
                      className="group-actions"
                      title={`Manage ${g.section.name}`}
                      aria-label={`Manage list ${g.section.name}`}
                      onClick={(event) => {
                        event.stopPropagation();
                        const rect = event.currentTarget.getBoundingClientRect();
                        setSectionMenu({
                          x: Math.max(8, rect.right - 210),
                          y: rect.bottom + 4,
                          id: g.section!.id,
                        });
                      }}
                    >
                      <span aria-hidden>•••</span>
                    </button>
                  )}
                </div>
              )}
              {g.items.map((item) => (
                <ItemCard
                  key={item.id}
                  item={item}
                  selected={selectedSet.has(item.id)}
                  editing={editingId === item.id}
                  expanded={expandedSet.has(item.id)}
                  settling={settlingSet.has(item.id)}
                  onClick={onItemClick}
                  onContextMenu={onItemContextMenu}
                  onToggleDone={onToggleItemDone}
                  onDoubleClick={onEditItem}
                  onEditSave={onSaveItemEdit}
                  onEditCancel={onCancelItemEdit}
                  onOpenAttachment={onOpenAttachment}
                  selectedAttachmentIds={selectedAttachmentSet}
                  getSelectedAttachmentPaths={getSelectedAttachmentPaths}
                  onSelectAttachment={onSelectAttachment}
                />
              ))}
              {g.section && g.items.length === 0 && (
                <div className="group-empty">Captures will land here</div>
              )}
            </div>
          ))
        )}
        {state && settledItems.length > 0 && (
          <div className="settled-block">
            <div className="settled-header">
              <span>Settled</span>
              <span className="settled-rule" />
              <button
                className="settled-clear"
                onClick={(event) => {
                  event.stopPropagation();
                  requestClearSettled(currentScope, settledIdsInScope);
                }}
              >
                Clear
              </button>
            </div>
            {settledItems.map((item) => (
              <ItemCard
                key={item.id}
                item={item}
                selected={selectedSet.has(item.id)}
                editing={editingId === item.id}
                expanded={false}
                settling={settlingSet.has(item.id)}
                settledContext={
                  showAllLists
                    ? item.sectionId === null
                      ? "Inbox"
                      : sectionNameById.get(item.sectionId)
                    : undefined
                }
                onClick={onItemClick}
                onContextMenu={onItemContextMenu}
                onToggleDone={onToggleItemDone}
                onDoubleClick={onEditItem}
                onEditSave={onSaveItemEdit}
                onEditCancel={onCancelItemEdit}
                onOpenAttachment={onOpenAttachment}
                selectedAttachmentIds={selectedAttachmentSet}
                getSelectedAttachmentPaths={getSelectedAttachmentPaths}
                onSelectAttachment={onSelectAttachment}
              />
            ))}
          </div>
        )}
        {!isEmpty && query && flatIds.length === 0 && (
          <div className="no-results">No matches for “{query}”</div>
        )}
      </div>

      {toast && (
        <div
          className={`toast${toast.error ? " error" : ""}${toastExiting ? " exiting" : ""}`}
          role="status"
          aria-live="polite"
        >
          <span className="toast-icon" aria-hidden>{toast.error ? "!" : "✓"}</span>
          <span>{toast.message}</span>
        </div>
      )}

      <div
        ref={composerRef}
        className={`composer${draggingFiles || draggingText ? " drop-active" : ""}`}
        onDragEnter={(event) => {
          const types = Array.from(event.dataTransfer.types);
          if (!types.includes("Files") && types.includes("text/plain")) {
            event.preventDefault();
            setDraggingText(true);
          }
        }}
        onDragOver={(event) => {
          const types = Array.from(event.dataTransfer.types);
          if (!types.includes("Files") && types.includes("text/plain")) {
            event.preventDefault();
            event.dataTransfer.dropEffect = "copy";
            setDraggingText(true);
          }
        }}
        onDragLeave={(event) => {
          const nextTarget = event.relatedTarget as Node | null;
          if (!nextTarget || !event.currentTarget.contains(nextTarget)) setDraggingText(false);
        }}
        onDrop={(event) => {
          const types = Array.from(event.dataTransfer.types);
          if (types.includes("Files")) return;
          const droppedText = event.dataTransfer.getData("text/plain").trim();
          if (!droppedText) return;
          event.preventDefault();
          event.stopPropagation();
          appendDroppedText(droppedText);
        }}
      >
        {(draggingFiles || draggingText) && (
          <div className="composer-drop" role="status">
            {draggingFiles && (
              <svg viewBox="0 0 20 20" width="18" height="18" aria-hidden>
                <path d="M10 3v10m0-10L6.5 6.5M10 3l3.5 3.5M4 12.5v3A1.5 1.5 0 0 0 5.5 17h9a1.5 1.5 0 0 0 1.5-1.5v-3" />
              </svg>
            )}
            <span>{draggingText ? "Release to add text" : "Release to add files"}</span>
          </div>
        )}
        {attachments.length > 0 && (
          <div className="composer-attachments" aria-label={`${attachments.length} pending attachments`}>
            {attachments.map((attachment) => (
              <AttachmentTile
                key={attachment.path}
                attachment={attachment}
                draft
                onRemove={() => removeAttachment(attachment)}
              />
            ))}
          </div>
        )}
        <div className="composer-row">
          <button
            className="attach-button"
            title="Attach files"
            aria-label="Attach files"
            disabled={!state || attaching}
            onClick={() => void pickAttachments()}
          >
            {attaching ? (
              <span className="attach-spinner" aria-hidden />
            ) : (
              <svg viewBox="0 0 18 18" width="18" height="18" aria-hidden>
                <circle cx="9" cy="9" r="7" />
                <path d="M9 5.5v7M5.5 9h7" />
              </svg>
            )}
          </button>
          <textarea
            ref={inputRef}
            autoFocus
            rows={1}
            aria-label="Add a note, prompt, or task"
            placeholder="Add a note, type a prompt, or describe a task"
            disabled={!state}
            maxLength={100_000}
            value={input}
            onChange={(e) => {
              setInput(e.target.value);
              e.target.style.height = "auto";
              e.target.style.height = `${Math.min(Math.max(e.target.scrollHeight, 34), 140)}px`;
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void submitInput();
              }
            }}
            onPaste={(event) => {
              const hasImage = Array.from(event.clipboardData.items).some(
                (item) => item.kind === "file" && item.type.startsWith("image/"),
              );
              if (!hasImage) return;
              event.preventDefault();
              void pasteClipboardImage();
            }}
          />
        </div>
        <div className="composer-meta">
          {activeSection && <span>Adding to {activeSection.name}</span>}
          {isListCommand(input) && (
            <span className={attachments.length ? "warning" : undefined}>
              {attachments.length ? "Remove files to create this list" : "Enter creates this list"}
            </span>
          )}
        </div>
      </div>

      {itemMenu && (
        <ContextMenu
          x={itemMenu.x}
          y={itemMenu.y}
          entries={menuEntries(itemMenu.ids)}
          onClose={() => setItemMenu(null)}
        />
      )}
      {sectionMenu && (
        <ContextMenu
          x={sectionMenu.x}
          y={sectionMenu.y}
          entries={sectionMenuEntries(sectionMenu.id)}
          onClose={() => setSectionMenu(null)}
        />
      )}
      {appMenu && (
        <ContextMenu
          x={appMenu.x}
          y={appMenu.y}
          entries={appMenuEntries()}
          onClose={() => setAppMenu(null)}
        />
      )}
      {showSwitcher && (
        <SectionSwitcher
          sections={sections}
          activeSectionId={activeSectionId}
          mode={movingIds ? "move" : "switch"}
          onSelect={(id) => {
            if (movingIds) {
              const ids = movingIds;
              setMovingIds(null);
              void api
                .moveItems(ids, id)
                .then(() => {
                  setSelected([]);
                  flashToast(`Moved ${ids.length} ${ids.length === 1 ? "prompt" : "prompts"}`);
                })
                .catch((cause) => reportError("Couldn’t move prompts", cause));
              return;
            }
            setShowAllLists(false);
            void api
              .setActiveSection(id)
              .catch((cause) => reportError("Couldn’t switch list", cause));
          }}
          onCreate={(name) => {
            if (movingIds) {
              const ids = movingIds;
              setMovingIds(null);
              void api
                .createSection(name)
                .then((id) => api.moveItems(ids, id))
                .then(() => {
                  setSelected([]);
                  setShowAllLists(false);
                  flashToast(`Moved ${ids.length} ${ids.length === 1 ? "prompt" : "prompts"}`);
                })
                .catch((cause) => reportError("Couldn’t move prompts", cause));
              return;
            }
            setShowAllLists(false);
            void api.createSection(name)
              .catch((cause) => reportError("Couldn’t create list", cause));
          }}
          onClose={() => {
            setShowSwitcher(false);
            setMovingIds(null);
          }}
        />
      )}
      {showHelp && state && (
        <ShortcutsSheet
          showShortcut={state.showShortcut}
          captureShortcut={state.captureShortcut}
          onClose={() => setShowHelp(false)}
        />
      )}
      {showSettings && state && (
        <SettingsSheet
          showShortcut={state.showShortcut}
          captureShortcut={state.captureShortcut}
          onSave={(showShortcut, captureShortcut) => api.setShortcuts(showShortcut, captureShortcut)}
          onClose={() => setShowSettings(false)}
        />
      )}
      {confirmClear && (
        <ConfirmDialog
          title={`Clear settled prompts from ${confirmClear.scope}?`}
          detail={`This permanently removes ${confirmClear.ids.length} settled ${confirmClear.ids.length === 1 ? "prompt" : "prompts"} and any locally stored attachment copies.`}
          confirmLabel="Clear settled"
          onCancel={() => setConfirmClear(null)}
          onConfirm={() => {
            const request = confirmClear;
            setConfirmClear(null);
            void api
              .deleteItems(request.ids)
              .then(() => {
                setSelected([]);
                setSelectedAttachments([]);
                flashToast(`Cleared settled prompts from ${request.scope}`);
              })
              .catch((cause) => reportError("Couldn’t clear settled prompts", cause));
          }}
        />
      )}
      {deletingList && (
        <ConfirmDialog
          title={`Delete “${deletingList.name}” and its prompts?`}
          detail="This permanently deletes every prompt in this list and all locally stored attachment copies."
          confirmLabel="Delete list and prompts"
          onCancel={() => setDeletingList(null)}
          onConfirm={() => {
            const section = deletingList;
            setDeletingList(null);
            void api
              .deleteSection(section.id, true)
              .then(() => {
                setSelected([]);
                setSelectedAttachments([]);
                flashToast(`Deleted “${section.name}” and its prompts`);
              })
              .catch((cause) => reportError("Couldn’t delete list", cause));
          }}
        />
      )}
    </div>
  );
}
