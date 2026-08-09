import { listen } from "@tauri-apps/api/event";
import {
  useCallback,
  useDeferredValue,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { api, useAppState } from "../store";
import type { Item, Section, Theme } from "../types";
import ConfirmDialog from "./ConfirmDialog";
import ContextMenu, { MenuEntry } from "./ContextMenu";
import ItemCard from "./ItemCard";
import SectionSwitcher from "./SectionSwitcher";
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

export default function Panel() {
  const { state, error } = useAppState();
  const [query, setQuery] = useState("");
  const [input, setInput] = useState("");
  const [selected, setSelected] = useState<number[]>([]);
  const [anchor, setAnchor] = useState<number | null>(null);
  const [editingId, setEditingId] = useState<number | null>(null);
  const [itemMenu, setItemMenu] = useState<{ x: number; y: number; ids: number[] } | null>(null);
  const [sectionMenu, setSectionMenu] = useState<{ x: number; y: number; id: number } | null>(null);
  const [renamingSection, setRenamingSection] = useState<number | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [showSwitcher, setShowSwitcher] = useState(false);
  const [showHelp, setShowHelp] = useState(false);
  const [appMenu, setAppMenu] = useState<{ x: number; y: number } | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [confirmClear, setConfirmClear] = useState(false);
  const [accessibilityGranted, setAccessibilityGranted] = useState<boolean | null>(null);

  const inputRef = useRef<HTMLTextAreaElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const toastTimer = useRef<number>();
  const scrollAfterAdd = useRef(false);
  const deferredQuery = useDeferredValue(query);

  const flashToast = useCallback((msg: string) => {
    setToast(msg);
    window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), 2200);
  }, []);

  useEffect(() => () => window.clearTimeout(toastTimer.current), []);

  const reportError = useCallback(
    (action: string, cause: unknown) => {
      const detail = cause instanceof Error ? cause.message : String(cause);
      flashToast(`${action}: ${detail}`);
    },
    [flashToast],
  );

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
        flashToast("Finish setup in System Settings, then return to Cooper");
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
      inputRef.current?.focus();
      refreshAccessibility();
    });
    addListener("captured", () => flashToast("Captured"));
    addListener("capture-empty", () => flashToast("No text selected"));
    addListener("capture-duplicate", () => flashToast("Already captured"));
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
  }, [flashToast, refreshAccessibility, reportError]);

  const sections = state?.sections ?? [];
  const items = state?.items ?? [];
  const activeSectionId = state?.activeSectionId ?? null;
  const activeSection = sections.find((s) => s.id === activeSectionId) ?? null;

  const groups = useMemo<Group[]>(() => {
    const q = deferredQuery.trim().toLocaleLowerCase();
    const buckets = new Map<number | null, Item[]>();
    for (const item of items) {
      if (q && !item.content.toLocaleLowerCase().includes(q)) continue;
      const bucket = buckets.get(item.sectionId);
      if (bucket) bucket.push(item);
      else buckets.set(item.sectionId, [item]);
    }
    const result: Group[] = [];
    const unfiled = buckets.get(null) ?? [];
    if (unfiled.length) result.push({ section: null, items: unfiled });
    for (const s of sections) {
      const inSection = buckets.get(s.id) ?? [];
      // Show an empty section header only when it's the active target (and not searching).
      if (inSection.length || (!q && s.id === activeSectionId)) {
        result.push({ section: s, items: inSection });
      }
    }
    return result;
  }, [items, sections, deferredQuery, activeSectionId]);

  const flatIds = useMemo(() => groups.flatMap((g) => g.items.map((i) => i.id)), [groups]);
  const itemById = useMemo(() => new Map(items.map((i) => [i.id, i])), [items]);
  const selectedSet = useMemo(() => new Set(selected), [selected]);
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
  }, [itemById]);

  useEffect(() => {
    if (!scrollAfterAdd.current) return;
    scrollAfterAdd.current = false;
    requestAnimationFrame(() => {
      listRef.current?.scrollTo({ top: listRef.current.scrollHeight });
    });
  }, [items.length]);

  const copySelection = useCallback(
    (asList: boolean, ids?: number[]) => {
      const targets = (ids ?? selected)
        .slice()
        .sort((a, b) => (orderById.get(a) ?? 0) - (orderById.get(b) ?? 0))
        .map((id) => itemById.get(id))
        .filter((i): i is Item => !!i);
      if (!targets.length) return;
      const text = asList
        ? targets.map((i) => `- ${i.content.replace(/\n/g, "\n  ")}`).join("\n")
        : targets.map((i) => i.content).join("\n\n");
      void api
        .copyText(text)
        .then(() => flashToast(asList ? "Copied as list" : "Copied"))
        .catch((cause) => reportError("Couldn’t copy", cause));
    },
    [selected, orderById, itemById, flashToast, reportError],
  );

  const deleteIds = useCallback(
    async (ids: number[]) => {
      try {
        await api.deleteItems(ids);
        setSelected([]);
      } catch (cause) {
        reportError("Couldn’t delete", cause);
      }
    },
    [reportError],
  );

  const setIdsDone = useCallback(
    (ids: number[]) => {
      const targets = ids.map((id) => itemById.get(id)).filter((item): item is Item => !!item);
      const done = !targets.every((item) => item.done);
      void api.setItemsDone(ids, done).catch((cause) => reportError("Couldn’t update", cause));
    },
    [itemById, reportError],
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
      setAnchor(id);
    }
  }, []);

  const onItemContextMenu = useCallback((e: React.MouseEvent, id: number) => {
    e.preventDefault();
    const ids = selectedSetRef.current.has(id) ? selectedRef.current : [id];
    if (!selectedSetRef.current.has(id)) {
      setSelected([id]);
      setAnchor(id);
    }
    setItemMenu({ x: e.clientX, y: e.clientY, ids });
  }, []);

  const onToggleItemDone = useCallback((id: number) => {
    setIdsDoneRef.current([id]);
  }, []);

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
      if (confirmClear || showHelp || showSwitcher || itemMenu || sectionMenu || appMenu) return;

      if (mod && e.key.toLowerCase() === "f") {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
        return;
      }
      if (mod && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setShowSwitcher((v) => !v);
        return;
      }
      if (e.key === "Escape") {
        if (editingId !== null) return;
        if (inField && (target as HTMLTextAreaElement).value) return;
        if (selected.length) {
          setSelected([]);
          return;
        }
        void api.hidePanel().catch((cause) => reportError("Couldn’t hide Cooper", cause));
        return;
      }
      if (mod && e.key.toLowerCase() === "w") {
        e.preventDefault();
        void api.hidePanel().catch((cause) => reportError("Couldn’t hide Cooper", cause));
        return;
      }
      if (inField || editingId !== null || showSwitcher || showHelp) return;

      if (mod && e.key.toLowerCase() === "a") {
        e.preventDefault();
        setSelected(flatIds);
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
      } else if (mod && !e.shiftKey && e.key.toLowerCase() === "c" && selected.length) {
        if (window.getSelection()?.toString()) return;
        e.preventDefault();
        copySelection(false);
      } else if (mod && e.shiftKey && e.key.toLowerCase() === "c" && selected.length) {
        e.preventDefault();
        copySelection(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [
    flatIds,
    anchor,
    selected,
    editingId,
    showSwitcher,
    showHelp,
    itemMenu,
    sectionMenu,
    appMenu,
    confirmClear,
    copySelection,
    deleteIds,
    setIdsDone,
    reportError,
  ]);

  const submitInput = async () => {
    const text = input.trim();
    if (!text) return;
    try {
      scrollAfterAdd.current = !text.startsWith("# ");
      await api.addEntry(text);
      setInput("");
      if (inputRef.current) inputRef.current.style.height = "auto";
      if (text.startsWith("# ")) {
        flashToast(`Section “${text.slice(2).trim()}”`);
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
        label: allDone ? "Mark as Not Done" : "Mark as Done",
        kbd: "Space",
        separatorBefore: true,
        onClick: () => setIdsDone(ids),
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
          .catch((cause) => reportError("Couldn’t switch section", cause)),
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
      label: "Delete section",
      danger: true,
      separatorBefore: true,
      onClick: () =>
        void api
          .deleteSection(id)
          .then(() => flashToast("Section removed · notes moved to Inbox"))
          .catch((cause) => reportError("Couldn’t remove section", cause)),
    },
  ];

  const appMenuEntries = (): MenuEntry[] => {
    const theme = state?.theme ?? "system";
    const entries: MenuEntry[] = [
      { label: "New section…", kbd: `${MOD} K`, onClick: () => setShowSwitcher(true) },
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
        label: "Clear completed",
        danger: true,
        disabled: !items.some((item) => item.done),
        onClick: () => setConfirmClear(true),
      },
      { label: "Shortcuts", kbd: `${MOD} /`, separatorBefore: true, onClick: () => setShowHelp(true) },
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
    <div className="panel" aria-busy={!state}>
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
          title="Cooper menu"
          aria-label="Open Cooper menu"
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
          if (e.target === e.currentTarget) setSelected([]);
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
                <span>Show Cooper</span>
                <span>
                  <kbd>Right Shift</kbd> <kbd>Right Shift</kbd>
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
          groups.map((g) => (
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
                            reportError("Couldn’t rename section", cause);
                          }
                        } else if (e.key === "Escape") {
                          setRenamingSection(null);
                        }
                      }}
                      onBlur={() => setRenamingSection(null)}
                    />
                  ) : (
                    <span
                      className={`group-name${g.section.id === activeSectionId ? " active" : ""}`}
                      title={
                        g.section.id === activeSectionId
                          ? "New captures land here"
                          : "Right-click for options"
                      }
                    >
                      {g.section.id === activeSectionId && <span className="active-dot" />}
                      {g.section.name}
                    </span>
                  )}
                  <span className="group-rule" />
                </div>
              )}
              {g.items.map((item) => (
                <ItemCard
                  key={item.id}
                  item={item}
                  selected={selectedSet.has(item.id)}
                  editing={editingId === item.id}
                  onClick={onItemClick}
                  onContextMenu={onItemContextMenu}
                  onToggleDone={onToggleItemDone}
                  onDoubleClick={onEditItem}
                  onEditSave={onSaveItemEdit}
                  onEditCancel={onCancelItemEdit}
                />
              ))}
              {g.section && g.items.length === 0 && (
                <div className="group-empty">Captures will land here</div>
              )}
            </div>
          ))
        )}
        {!isEmpty && query && flatIds.length === 0 && (
          <div className="no-results">No matches for “{query}”</div>
        )}
      </div>

      {toast && (
        <div className="toast" role="status" aria-live="polite">
          {toast}
        </div>
      )}

      <div className="composer">
        <textarea
          ref={inputRef}
          autoFocus
          rows={1}
          aria-label="Add a note or prompt"
          placeholder={`Add a note or a prompt${activeSection ? ` (${activeSection.name.toLowerCase()})` : ""}`}
          disabled={!state}
          maxLength={100_000}
          value={input}
          onChange={(e) => {
            setInput(e.target.value);
            e.target.style.height = "auto";
            e.target.style.height = `${Math.min(e.target.scrollHeight, 120)}px`;
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              submitInput();
            }
          }}
        />
        {input.startsWith("# ") && <span className="composer-hint">creates a section</span>}
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
          onSelect={(id) => {
            void api
              .setActiveSection(id)
              .catch((cause) => reportError("Couldn’t switch section", cause));
          }}
          onCreate={(name) => {
            void api
              .createSection(name)
              .catch((cause) => reportError("Couldn’t create section", cause));
          }}
          onClose={() => setShowSwitcher(false)}
        />
      )}
      {showHelp && <ShortcutsSheet onClose={() => setShowHelp(false)} />}
      {confirmClear && (
        <ConfirmDialog
          title="Clear completed notes?"
          detail="This permanently removes every completed note."
          confirmLabel="Clear completed"
          onCancel={() => setConfirmClear(false)}
          onConfirm={() => {
            setConfirmClear(false);
            void api
              .clearCompleted()
              .then(() => {
                setSelected([]);
                flashToast("Completed notes cleared");
              })
              .catch((cause) => reportError("Couldn’t clear completed notes", cause));
          }}
        />
      )}
    </div>
  );
}
