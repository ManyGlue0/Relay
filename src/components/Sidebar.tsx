import { AnimatePresence, motion } from "framer-motion";
import {
  Archive,
  ArchiveRestore,
  ArrowLeft,
  BarChart3,
  BookMarked,
  ChevronRight,
  Ellipsis,
  FolderClosed,
  Info,
  MessageSquareDashed,
  Pencil,
  Pin,
  PinOff,
  Search,
  SearchX,
  Server,
  Settings2,
  SlidersHorizontal,
  SquarePen,
  Trash2,
  UserRound,
  Wrench,
  X,
  type LucideIcon,
} from "lucide-react";
import { forwardRef, useEffect, useMemo, useRef, useState } from "react";
import type { ChatSummary, SearchHit } from "../lib/api";
import { rowSpring, spring } from "../lib/motion";
import { useStore, useView, type SettingsSection } from "../lib/store";
import { folderName } from "../lib/folder";
import { dateGroup } from "../lib/time";
import { MenuItem, MenuSeparator, Pill, Popover, type Anchor } from "./ui";

export const SECTIONS: { id: SettingsSection; label: string; icon: LucideIcon }[] = [
  { id: "general", label: "General", icon: SlidersHorizontal },
  { id: "you", label: "You", icon: UserRound },
  { id: "models", label: "Models", icon: Server },
  { id: "presets", label: "Presets", icon: Settings2 },
  { id: "tools", label: "Tools", icon: Wrench },
  { id: "skills", label: "Skills", icon: BookMarked },
  { id: "stats", label: "Stats", icon: BarChart3 },
  { id: "about", label: "About", icon: Info },
];

/** One sidebar, open or collapsed. Collapsing only narrows it: every icon
 *  stays where it is, rows shrink to their icon, and the rest fades. */
export const Sidebar = forwardRef<HTMLInputElement, { rail: boolean }>(function Sidebar({ rail }, searchRef) {
  const settingsOpen = useStore((s) => s.settingsOpen);
  return <aside className={`sidebar ${rail ? "rail" : ""}`}>{settingsOpen ? <SettingsNav rail={rail} /> : <ChatsNav rail={rail} ref={searchRef} />}</aside>;
});

function SideRow({
  icon: Icon,
  label,
  keys,
  onClick,
  on,
  pill,
  rail,
}: {
  icon: LucideIcon;
  label: string;
  keys?: string;
  onClick: () => void;
  on?: boolean;
  /** layoutId of the selection pill, when this row can be the selected one. */
  pill?: string;
  rail: boolean;
}) {
  return (
    <motion.button
      className={`side-row ${on ? "on" : ""}`}
      onClick={onClick}
      // A row that can hold the selection pill doesn't scale on press: the pill
      // would be measured mid-press and slide from the wrong place.
      whileTap={pill ? undefined : { scale: 0.97 }}
      transition={spring}
      aria-label={label}
      title={rail ? (keys ? `${label} (${keys})` : label) : undefined}
    >
      {on && pill && <Pill layoutId={pill} className="row-pill" />}
      <Icon size={16} className="side-icon" />
      <span className="side-label">{label}</span>
      {keys && <span className="side-keys">{keys}</span>}
    </motion.button>
  );
}

const ChatsNav = forwardRef<HTMLInputElement, { rail: boolean }>(function ChatsNav({ rail }, searchRef) {
  const query = useStore((s) => s.query);
  const hits = useStore((s) => s.hits);
  const agent = useView() === "agent";
  const things = agent ? "sessions" : "chats";
  return (
    <>
      <div className="side-actions">
        <SideRow icon={SquarePen} label={agent ? "New session" : "New chat"} keys="Ctrl+N" rail={rail} onClick={() => useStore.getState().newChat()} />
        <div className="search" onClick={() => rail && openSearch()} title={rail ? `Search ${things} (Ctrl+K)` : undefined}>
          <Search size={16} className="search-icon" />
          <input
            ref={searchRef}
            placeholder={`Search ${things}`}
            value={query}
            tabIndex={rail ? -1 : 0}
            onChange={(e) => useStore.getState().setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape" && query) {
                e.preventDefault();
                e.stopPropagation();
                useStore.getState().setQuery("");
              }
            }}
          />
          {query && !rail && (
            <button className="search-clear" aria-label="Clear search" onClick={() => useStore.getState().setQuery("")}>
              <X size={13} />
            </button>
          )}
        </div>
      </div>
      <div className="chat-list fades">{hits ? <Results hits={hits} /> : <ChatList />}</div>
      <div className="sidebar-foot">
        <SideRow icon={Settings2} label="Settings" keys="Ctrl+," rail={rail} onClick={() => useStore.getState().openSettings()} />
      </div>
    </>
  );
});

/** From the rail: open the sidebar and put the cursor in the search box. */
function openSearch() {
  void useStore.getState().updateSettings({ sidebarOpen: true });
  setTimeout(() => document.querySelector<HTMLInputElement>(".search input")?.focus(), 80);
}

/** While Settings is open, the sidebar lists its sections. */
function SettingsNav({ rail }: { rail: boolean }) {
  const section = useStore((s) => s.section);
  const agent = useView() === "agent";
  const go = (id: SettingsSection) => useStore.setState({ section: id, editing: null });
  return (
    <>
      <div className="side-actions">
        <SideRow icon={ArrowLeft} label={agent ? "Back to sessions" : "Back to chats"} keys="Esc" rail={rail} onClick={() => useStore.getState().closeSettings()} />
      </div>
      <div className="group-label nav-label fades">Settings</div>
      <nav className="settings-nav">
        {SECTIONS.map((s) => (
          <SideRow key={s.id} icon={s.icon} label={s.label} rail={rail} on={section === s.id} pill="nav-pill" onClick={() => go(s.id)} />
        ))}
      </nav>
    </>
  );
}

interface Group {
  key: string;
  label: string;
  /** A folder group's full path. */
  title?: string;
  folder?: boolean;
  chats: ChatSummary[];
}

/** Chats by date; Agent sessions by folder (the workspaces Relay made
 *  share one group), newest first. Pinned ones on top either way. */
function groupChats(chats: ChatSummary[], agent: boolean): Group[] {
  const out: Group[] = [];
  const now = new Date();
  if (!agent) {
    for (const c of chats) {
      const label = c.pinned ? "Pinned" : dateGroup(c.updatedAt, now);
      const last = out[out.length - 1];
      if (last?.label === label) last.chats.push(c);
      else out.push({ key: label, label, chats: [c] });
    }
    return out;
  }
  const byKey = new Map<string, Group>();
  for (const c of chats) {
    const key = c.pinned ? "pinned" : c.folderOwned || !c.folder ? "workspaces" : c.folder.toLowerCase();
    let g = byKey.get(key);
    if (!g) {
      g =
        key === "pinned"
          ? { key, label: "Pinned", chats: [] }
          : key === "workspaces"
            ? { key, label: "Workspaces", title: "Folders Relay made for sessions without one", folder: true, chats: [] }
            : { key, label: folderName(c.folder!), title: c.folder!, folder: true, chats: [] };
      byKey.set(key, g);
      out.push(g);
    }
    g.chats.push(c);
  }
  return out;
}

function ChatList() {
  const all = useStore((s) => s.chats);
  const view = useView();
  const agent = view === "agent";
  const chats = useMemo(() => all.filter((c) => c.kind === view && !c.archived), [all, view]);
  const archived = useMemo(() => all.filter((c) => c.kind === view && c.archived), [all, view]);
  const groups = useMemo(() => groupChats(chats, agent), [chats, agent]);

  if (!chats.length && !archived.length) {
    return (
      <div className="list-empty">
        {agent ? <FolderClosed size={18} /> : <MessageSquareDashed size={18} />}
        <span>{agent ? "Your sessions will show up here" : "Your chats will show up here"}</span>
      </div>
    );
  }
  return (
    <>
      {archived.length > 0 && <ArchivedGroup key={view} chats={archived} />}
      {groups.map((g) => (
        <section key={g.key} className="chat-group">
          <div className={`group-label ${g.folder ? "folder-label" : ""}`} title={g.title}>
            {g.label}
          </div>
          <AnimatePresence initial={false}>
            {g.chats.map((c) => (
              <ChatRow key={c.id} chat={c} />
            ))}
          </AnimatePresence>
        </section>
      ))}
    </>
  );
}

/** Archived chats: one folded row under the search box. */
function ArchivedGroup({ chats }: { chats: ChatSummary[] }) {
  const currentId = useStore((s) => s.currentId);
  const [open, setOpen] = useState(() => chats.some((c) => c.id === currentId));
  // Opening an archived chat (from search, or Holster) unfolds the group.
  useEffect(() => {
    if (chats.some((c) => c.id === currentId)) setOpen(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentId]);
  return (
    <section className="chat-group archived-group">
      <button className="side-row archived-head" aria-expanded={open} onClick={() => setOpen(!open)}>
        <Archive size={16} className="side-icon" />
        <span className="side-label">Archived</span>
        <span className="archived-count">{chats.length}</span>
        <ChevronRight size={14} className={`archived-caret ${open ? "open" : ""}`} />
      </button>
      <AnimatePresence initial={false}>
        {open && (
          <motion.div
            className="archived-list"
            style={{ overflow: "hidden" }}
            initial={{ height: 0, opacity: 0 }}
            // Clipped only while it folds: the selection pill travels between rows.
            animate={{ height: "auto", opacity: 1, transitionEnd: { overflow: "visible" } }}
            exit={{ height: 0, opacity: 0, overflow: "hidden" }}
            transition={spring}
          >
            <AnimatePresence initial={false}>
              {chats.map((c) => (
                <ChatRow key={c.id} chat={c} />
              ))}
            </AnimatePresence>
          </motion.div>
        )}
      </AnimatePresence>
    </section>
  );
}

function ChatRow({ chat }: { chat: ChatSummary }) {
  const active = useStore((s) => s.currentId === chat.id && !s.settingsOpen);
  const streaming = useStore((s) => !!s.streaming[chat.id]);
  const waiting = useStore((s) => !!s.waiting[chat.id]);
  const [menu, setMenu] = useState<Anchor | null>(null);
  const [renaming, setRenaming] = useState(false);
  /** Deleting: first the session, then its workspace (one Relay made) or its checkpoints. */
  const [confirming, setConfirming] = useState<false | "chat" | "workspace" | "checkpoints">(false);
  const more = useRef<HTMLButtonElement>(null);
  const session = chat.kind === "agent";

  if (confirming === "workspace") {
    return (
      <motion.div key="ws" layout className="chat-row confirm" initial={{ opacity: 0 }} animate={{ opacity: 1 }} transition={rowSpring}>
        <span className="confirm-text" title={chat.folder ?? undefined}>
          Its workspace too?
        </span>
        <button className="confirm-yes" onClick={() => void useStore.getState().deleteChat(chat.id, true)}>
          Delete
        </button>
        <button className="confirm-no" autoFocus title="Delete the session, keep its folder" onClick={() => void useStore.getState().deleteChat(chat.id, false)}>
          Keep
        </button>
      </motion.div>
    );
  }
  if (confirming === "checkpoints") {
    return (
      <motion.div key="cp" layout className="chat-row confirm" initial={{ opacity: 0 }} animate={{ opacity: 1 }} transition={rowSpring}>
        <span className="confirm-text" title="Kept, they stay as the folder's file history">
          Its checkpoints too?
        </span>
        <button className="confirm-yes" onClick={() => void useStore.getState().deleteChat(chat.id, false, true)}>
          Delete
        </button>
        <button className="confirm-no" autoFocus title="Keep them as the folder's file history" onClick={() => void useStore.getState().deleteChat(chat.id, false, false)}>
          Keep
        </button>
      </motion.div>
    );
  }
  if (confirming) {
    return (
      <motion.div layout className="chat-row confirm" initial={{ opacity: 0 }} animate={{ opacity: 1 }} transition={rowSpring}>
        <span className="confirm-text">{session ? "Delete this session?" : "Delete this chat?"}</span>
        <button
          className="confirm-yes"
          onClick={() =>
            session && chat.folderOwned && chat.folder ? setConfirming("workspace") : session && chat.folder ? setConfirming("checkpoints") : void useStore.getState().deleteChat(chat.id)
          }
        >
          Delete
        </button>
        <button className="confirm-no" autoFocus onClick={() => setConfirming(false)}>
          Cancel
        </button>
      </motion.div>
    );
  }

  return (
    <motion.div
      layout="position"
      initial={{ opacity: 0, x: -10 }}
      animate={{ opacity: 1, x: 0 }}
      exit={{ opacity: 0, x: -10, transition: { duration: 0.12 } }}
      transition={rowSpring}
      className={`chat-row ${active ? "active" : ""} ${menu ? "menu-open" : ""}`}
      onClick={() => !renaming && void useStore.getState().openChat(chat.id)}
      onDoubleClick={() => setRenaming(true)}
      onContextMenu={(e) => {
        e.preventDefault();
        setMenu({ x: e.clientX, y: e.clientY });
      }}
      role="button"
      aria-current={active}
    >
      {active && <Pill layoutId="chat-pill" className="row-pill" />}
      {renaming ? <Rename chat={chat} onDone={() => setRenaming(false)} /> : <span className="chat-title">{chat.title || (session ? "New session" : "New chat")}</span>}
      {streaming && <span className={`live-dot ${waiting ? "asks" : ""}`} aria-label={waiting ? "Waiting for you" : "Replying"} title={waiting ? "Waiting for you" : undefined} />}
      {!renaming && (
        <button
          ref={more}
          className="row-more"
          aria-label="More"
          onClick={(e) => {
            e.stopPropagation();
            const r = more.current!.getBoundingClientRect();
            setMenu(menu ? null : { x: Math.round(r.right), y: Math.round(r.bottom + 4), alignRight: true });
          }}
        >
          <Ellipsis size={15} />
        </button>
      )}
      <Popover anchor={menu} onClose={() => setMenu(null)} width={190} trigger={more}>
        <div className="menu" onClick={(e) => e.stopPropagation()}>
          <MenuItem
            icon={<Pencil size={15} />}
            onSelect={() => {
              setMenu(null);
              setRenaming(true);
            }}
          >
            Rename
          </MenuItem>
          <MenuItem
            icon={chat.pinned ? <PinOff size={15} /> : <Pin size={15} />}
            onSelect={() => {
              setMenu(null);
              void useStore.getState().pinChat(chat.id, !chat.pinned);
            }}
          >
            {chat.pinned ? "Unpin" : "Pin"}
          </MenuItem>
          <MenuItem
            icon={chat.archived ? <ArchiveRestore size={15} /> : <Archive size={15} />}
            onSelect={() => {
              setMenu(null);
              void useStore.getState().archiveChat(chat.id, !chat.archived);
            }}
          >
            {chat.archived ? "Unarchive" : "Archive"}
          </MenuItem>
          <MenuSeparator />
          <MenuItem
            danger
            icon={<Trash2 size={15} />}
            onSelect={() => {
              setMenu(null);
              setConfirming("chat");
            }}
          >
            Delete…
          </MenuItem>
        </div>
      </Popover>
    </motion.div>
  );
}

function Rename({ chat, onDone }: { chat: ChatSummary; onDone: () => void }) {
  const [value, setValue] = useState(chat.title);
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    ref.current?.select();
  }, []);
  const commit = () => {
    if (value.trim() && value.trim() !== chat.title) void useStore.getState().renameChat(chat.id, value);
    onDone();
  };
  return (
    <input
      ref={ref}
      className="rename"
      value={value}
      autoFocus
      onClick={(e) => e.stopPropagation()}
      onChange={(e) => setValue(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") commit();
        if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          onDone();
        }
      }}
    />
  );
}

function Results({ hits }: { hits: SearchHit[] }) {
  const currentId = useStore((s) => s.currentId);
  const agent = useView() === "agent";
  const [one, many] = agent ? ["session", "sessions"] : ["chat", "chats"];
  if (!hits.length) {
    return (
      <div className="list-empty">
        <SearchX size={18} />
        <span>No {many} match</span>
      </div>
    );
  }
  return (
    <section className="chat-group">
      <div className="group-label">{hits.length === 1 ? `1 ${one}` : `${hits.length} ${many}`}</div>
      {hits.map((h, i) => (
        <motion.button
          key={h.chatId}
          className={`hit ${currentId === h.chatId ? "active" : ""}`}
          initial={{ opacity: 0, x: -8 }}
          animate={{ opacity: 1, x: 0 }}
          transition={{ ...rowSpring, delay: Math.min(i, 8) * 0.03 }}
          onClick={() => void useStore.getState().openChat(h.chatId)}
        >
          <span className="hit-head">
            <span className="hit-title">{h.title || (agent ? "New session" : "New chat")}</span>
            {h.archived && <span className="hit-tag">Archived</span>}
          </span>
          {h.snippet && <span className="hit-snippet">{highlighted(h.snippet)}</span>}
        </motion.button>
      ))}
    </section>
  );
}

/** FTS snippets mark hits with \u0001…\u0002. */
function highlighted(snippet: string) {
  return snippet.split(/(\u0001[^\u0002]*\u0002)/g).map((part, i) =>
    part.startsWith("\u0001") ? <mark key={i}>{part.slice(1, -1)}</mark> : <span key={i}>{part}</span>,
  );
}
