import { getCurrentWindow } from "@tauri-apps/api/window";
import { AnimatePresence, motion } from "framer-motion";
import { Activity, Ban, Bot, Copy as RestoreIcon, FileStack, FolderClosed, GitBranch, MessageCircle, Minus, PanelLeft, Play, Square, Trash2, X } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import { api, errorText, type BlockedStep, type ChatKind, type ProcessView } from "../lib/api";
import { folderName, useFolderInfo } from "../lib/folder";
import { spring } from "../lib/motion";
import { useStore, useView } from "../lib/store";
import { SECTIONS } from "./Sidebar";
import { IconButton, MenuItem, MenuSeparator, Pill, Popover, anchorFrom, type Anchor } from "./ui";

/** The collapsed sidebar's width: its icons sit on the same column as the open sidebar's. */
export const RAIL = 54;
/** The title bar's fixed left group (mark, name, sidebar button). */
const BRAND = 132;

export function TitleBar({ sidebarWidth }: { sidebarWidth: number }) {
  const maximized = useStore((s) => s.maximized);
  const open = useStore((s) => s.settings?.sidebarOpen ?? true);
  const settingsOpen = useStore((s) => s.settingsOpen);
  const section = useStore((s) => s.section);
  const chatTitle = useStore((s) => (s.currentId ? (s.chats.find((c) => c.id === s.currentId)?.title ?? s.current?.title ?? "") : ""));
  const title = settingsOpen ? `Settings · ${SECTIONS.find((x) => x.id === section)?.label ?? ""}` : chatTitle;
  // The title starts where the conversation does, and travels with it.
  const titleOffset = Math.max(4, (open ? sidebarWidth : RAIL) + 16 - BRAND);

  return (
    <header className="titlebar" data-tauri-drag-region>
      <div className="brand" style={{ width: BRAND }} data-tauri-drag-region>
        <BrandMark />
        <span className="wordmark" data-tauri-drag-region>
          Relay
        </span>
        <IconButton label={open ? "Collapse the sidebar (Ctrl+B)" : "Expand the sidebar (Ctrl+B)"} onClick={() => void useStore.getState().updateSettings({ sidebarOpen: !open })}>
          <PanelLeft size={16} />
        </IconButton>
      </div>
      <AnimatePresence initial={false}>{open && <ViewSwitch key="switch" />}</AnimatePresence>
      <div className="titlebar-title" style={{ marginLeft: titleOffset }} data-tauri-drag-region>
        <span data-tauri-drag-region>{title}</span>
        {!settingsOpen && <SessionMeta />}
      </div>
      {!settingsOpen && <BlockedChip />}
      {!settingsOpen && <ProcessesChip />}
      {!settingsOpen && <FilesToggle />}
      <div className="caption">
        <button className="caption-btn" aria-label="Minimize" onClick={() => void getCurrentWindow().minimize()}>
          <Minus size={15} strokeWidth={1.6} />
        </button>
        <button className="caption-btn" aria-label={maximized ? "Restore" : "Maximize"} onClick={() => void getCurrentWindow().toggleMaximize()}>
          {maximized ? <RestoreIcon size={12} strokeWidth={1.7} style={{ transform: "scaleX(-1)" }} /> : <Square size={12} strokeWidth={1.7} />}
        </button>
        <button className="caption-btn close" aria-label="Close" onClick={() => void api.closeWindow()}>
          <X size={16} strokeWidth={1.6} />
        </button>
      </div>
    </header>
  );
}

/** An Agent session's folder and git branch, after its title. */
function SessionMeta() {
  const chat = useStore((s) => (s.current && s.current.id === s.currentId && s.current.kind === "agent" ? s.current : null));
  const info = useFolderInfo(chat?.folder);
  if (!chat?.folder) return null;
  const missing = info !== null && !info.exists;
  return (
    <span className={`title-meta ${missing ? "missing" : ""}`} title={missing ? `${chat.folder} no longer exists` : chat.folder} data-tauri-drag-region>
      <FolderClosed size={13} />
      <span className="title-meta-text">{chat.folderOwned ? "Workspace" : (info?.name ?? folderName(chat.folder))}</span>
      {info?.branch && (
        <>
          <GitBranch size={13} />
          <span className="title-meta-text">{info.branch}</span>
        </>
      )}
    </span>
  );
}

/** The session's background processes, with Stop (shown while there are any). */
function ProcessesChip() {
  const chatId = useStore((s) => (s.current && s.current.id === s.currentId && s.current.kind === "agent" ? s.current.id : null));
  const busy = useStore((s) => (chatId ? !!s.streaming[chatId] : false));
  const [list, setList] = useState<ProcessView[]>([]);
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!chatId) return setList([]);
    let live = true;
    const load = () => void api.processes(chatId).then((l) => live && setList(l)).catch(() => {});
    load();
    const t = setInterval(load, anchor || busy ? 1500 : 4000);
    return () => {
      live = false;
      clearInterval(t);
    };
  }, [chatId, anchor, busy]);
  const running = list.filter((p) => p.running);
  if (!chatId || running.length === 0) return null;
  return (
    <>
      <button ref={btn} className={`chip proc-chip ${anchor ? "open" : ""}`} title="Background processes" onClick={() => (anchor ? setAnchor(null) : setAnchor(anchorFrom(btn.current!, { alignRight: true })))}>
        <Activity size={14} className="chip-icon" />
        <span className="chip-label">{running.length === 1 ? "1 running" : `${running.length} running`}</span>
      </button>
      <Popover anchor={anchor} onClose={() => setAnchor(null)} width={320} trigger={btn}>
        <div className="menu">
          {list.map((p) => (
            <MenuItem
              key={p.id}
              icon={<Activity size={15} />}
              disabled={!p.running}
              trailing={p.running ? "Stop" : p.code != null ? `Exit ${p.code}` : "Ended"}
              onSelect={() => {
                void api.stopProcess(chatId, p.id).then(() => api.processes(chatId).then(setList));
              }}
            >
              <span className="menu-two">
                <span className="mono-label">{p.command}</span>
                <span className="menu-sub">
                  {p.shell} · process {p.id}
                </span>
              </span>
            </MenuItem>
          ))}
        </div>
      </Popover>
    </>
  );
}

/** What a blocked step was, in a few words. */
function stepText(b: BlockedStep): string {
  const a = b.args as Record<string, string | undefined>;
  if (a.command) return a.command;
  const verb: Record<string, string> = { write_file: "Write", edit_file: "Edit", delete_path: "Delete", read_file: "Read", list: "List", search: "Search" };
  return `${verb[b.tool] ?? b.tool} ${a.path ?? a.pattern ?? ""}`.trim();
}

/** Auto's blocked list: "Blocked: n", each step with "Run it now". */
function BlockedChip() {
  const chatId = useStore((s) => (s.current && s.current.id === s.currentId && s.current.kind === "agent" ? s.current.id : null));
  const seq = useStore((s) => s.blockedSeq);
  const [list, setList] = useState<BlockedStep[]>([]);
  const [running, setRunning] = useState<number | null>(null);
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!chatId) return setList([]);
    let live = true;
    void api.blocked(chatId).then((l) => live && setList(l)).catch(() => {});
    return () => {
      live = false;
    };
  }, [chatId, seq]);
  const waiting = list.filter((b) => !b.ran);
  if (!chatId || list.length === 0) return null;
  const run = (b: BlockedStep) => {
    setRunning(b.at);
    void api
      .runBlocked(chatId, b.at)
      .then((done) => {
        useStore.getState().toast(done.error ? `It ran, with an error: ${(done.result ?? "").slice(0, 160)}` : "It ran. Tell the model, if it should know.");
        void useStore.getState().refreshCheckpoints(chatId);
      })
      .catch((e) => useStore.getState().toast(errorText(e)))
      .finally(() => setRunning(null));
  };
  return (
    <>
      <button
        ref={btn}
        className={`chip proc-chip blocked-chip ${waiting.length ? "has" : ""} ${anchor ? "open" : ""}`}
        title="Steps Auto refused"
        onClick={() => (anchor ? setAnchor(null) : setAnchor(anchorFrom(btn.current!, { alignRight: true })))}
      >
        <Ban size={14} className="chip-icon" />
        <span className="chip-label">Blocked: {waiting.length}</span>
      </button>
      <Popover anchor={anchor} onClose={() => setAnchor(null)} width={380} trigger={btn}>
        <div className="menu blocked-menu">
          {list
            .slice()
            .reverse()
            .map((b) => (
              <MenuItem key={b.at} icon={<Play size={15} />} disabled={b.ran || running !== null} trailing={b.ran ? "Ran" : running === b.at ? "Running…" : "Run it now"} onSelect={() => run(b)}>
                <span className="menu-two">
                  <span className="mono-label">{stepText(b)}</span>
                  <span className="menu-sub">
                    {b.rule}: {b.why}
                  </span>
                </span>
              </MenuItem>
            ))}
          <MenuSeparator />
          <MenuItem
            icon={<Trash2 size={15} />}
            onSelect={() => {
              setAnchor(null);
              void api.clearBlocked(chatId);
            }}
          >
            Clear the list
          </MenuItem>
        </div>
      </Popover>
    </>
  );
}

/** Opens the session's files panel. */
function FilesToggle() {
  const agent = useStore((s) => !!s.current && s.current.id === s.currentId && s.current.kind === "agent");
  const open = useStore((s) => s.filesPanel);
  if (!agent) return null;
  return (
    <IconButton label={open ? "Hide the session's files" : "The session's files: what it changed, with versions"} active={open} className="titlebar-files" onClick={() => useStore.setState({ filesPanel: !open })}>
      <FileStack size={16} />
    </IconButton>
  );
}

/** Which icon stands for Agent (picked on screen: see docs/design/agent-mode.md). */

/** Chat or Agent, beside the sidebar button (hidden while the sidebar is collapsed). */
function ViewSwitch() {
  const view = useView();
  const items: { value: ChatKind; label: string; icon: typeof Bot }[] = [
    { value: "chat", label: "Chat", icon: MessageCircle },
    { value: "agent", label: "Agent", icon: Bot },
  ];
  return (
    <motion.div
      className="segmented view-switch"
      style={{ left: BRAND + 2 }}
      role="tablist"
      initial={{ opacity: 0, scale: 0.9 }}
      animate={{ opacity: 1, scale: 1 }}
      exit={{ opacity: 0, scale: 0.9, transition: { duration: 0.1 } }}
      transition={spring}
    >
      {items.map(({ value, label, icon: Icon }) => (
        <button
          key={value}
          role="tab"
          aria-selected={view === value}
          aria-label={label}
          title={label}
          className={view === value ? "on" : ""}
          onClick={() => useStore.getState().setView(value)}
        >
          {view === value && <Pill layoutId="view-pill" className="segmented-pill" />}
          <span>
            <Icon size={16} />
          </span>
        </button>
      ))}
    </motion.div>
  );
}

/** The app mark, as in the icon (two bubbles, no background). */
export function BrandMark({ size = 20 }: { size?: number }) {
  const mask = `brand-gap-${useId().replace(/:/g, "")}`;
  return (
    <svg viewBox="112 132 800 800" width={size} height={size} aria-hidden>
      <defs>
        <mask id={mask} maskUnits="userSpaceOnUse" x="0" y="0" width="1024" height="1024">
          <rect width="1024" height="1024" fill="#fff" />
          <path
            d="M232 380h318c58 0 104 46 104 104v206c0 58-46 104-104 104H366l-122 100v-100h-12c-58 0-104-46-104-104V484c0-58 46-104 104-104z"
            fill="#000"
            stroke="#000"
            strokeWidth="64"
            strokeLinejoin="round"
          />
        </mask>
      </defs>
      <path
        mask={`url(#${mask})`}
        d="M474 150h318c58 0 104 46 104 104v206c0 58-46 104-104 104h-28v100L642 564H474c-58 0-104-46-104-104V254c0-58 46-104 104-104z"
        fill="#B2A2F5"
      />
      <path d="M232 380h318c58 0 104 46 104 104v206c0 58-46 104-104 104H366l-122 100v-100h-12c-58 0-104-46-104-104V484c0-58 46-104 104-104z" fill="#89BCF2" />
      <circle cx="282" cy="587" r="38" fill="#1E2F47" />
      <circle cx="391" cy="587" r="38" fill="#1E2F47" />
      <circle cx="500" cy="587" r="38" fill="#1E2F47" />
    </svg>
  );
}
