import { open } from "@tauri-apps/plugin-dialog";
import { AnimatePresence, motion } from "framer-motion";
import { BookMarked, ChevronDown, CircleAlert, FilePen, FolderClosed, FolderOpen, Globe, Hand, Mic, Paperclip, Plug, Plus, RefreshCw, ShieldOff, SlidersHorizontal, Square, X, type LucideIcon } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { api, errorText, type AgentMode } from "../lib/api";
import { folderName, useFolderInfo } from "../lib/folder";
import { clock, micError, record, type Recording } from "../lib/media";
import { spring } from "../lib/motion";
import { compact, draftKeyOf, shortModel, useActiveModel, useChatOptions, useEscape, useStore, viewOf, type DraftFile } from "../lib/store";
import { AttachmentTray } from "./Attachments";
import { ModelEffortPicker } from "./ModelPicker";
import { MenuItem, MenuSeparator, Popover, SubmenuItem, SwitchItem, anchorFrom, type Anchor } from "./ui";

/** Drafts survive switching chats (per chat, for this session). */
const drafts = new Map<string, string>();
const NO_FILES: DraftFile[] = [];
/** Voice messages stop by themselves after this long. */
const MAX_RECORDING = 10 * 60;

export function Composer({ chatId, streaming }: { chatId: string | null; streaming: boolean }) {
  const key = useStore((s) => draftKeyOf({ settings: s.settings, currentId: chatId }));
  const [text, setText] = useState(() => drafts.get(key) ?? "");
  const ref = useRef<HTMLTextAreaElement>(null);
  const model = useActiveModel();
  const sendKey = useStore((s) => s.settings?.sendKey ?? "enter");
  const focusTick = useStore((s) => s.focusComposer);
  const files = useStore((s) => s.files[key]) ?? NO_FILES;
  const [sending, setSending] = useState(false);
  const [recording, setRecording] = useState(false);
  const agent = useStore((s) => (s.current ? s.current.kind : (s.draftOptions.kind ?? viewOf(s))) === "agent");

  useEffect(() => {
    setText(drafts.get(key) ?? "");
    // A recording belongs to the chat it started in.
    return () => {
      live?.rec.cancel();
      live = null;
      setRecording(false);
    };
  }, [key]);

  useEffect(() => {
    ref.current?.focus();
  }, [focusTick, key]);

  useEffect(() => {
    if (model) useStore.getState().ensureContext(model);
  }, [model]);

  // Grow with the text, up to 40% of the window.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "0px";
    el.style.height = `${Math.min(el.scrollHeight, Math.round(window.innerHeight * 0.4))}px`;
  }, [text]);

  const update = (v: string) => {
    setText(v);
    if (v) drafts.set(key, v);
    else drafts.delete(key);
  };

  const preparing = files.some((f) => !f.ready);
  const canSend = (!!text.trim() || files.length > 0) && !preparing && !recording && !!model && !streaming && !sending;

  const submit = async () => {
    if (!canSend) return;
    setSending(true);
    const ok = await useStore.getState().send(text);
    setSending(false);
    if (ok) {
      drafts.delete(key);
      setText("");
    }
  };

  return (
    <div className="composer">
      {files.length > 0 && <AttachmentTray files={files} />}
      <AnimatePresence initial={false}>{recording && <RecordingStrip key="rec" onDone={() => setRecording(false)} />}</AnimatePresence>
      <textarea
        ref={ref}
        rows={1}
        value={text}
        placeholder={model ? (files.length ? "Add a message, or just send" : agent ? "What should we work on?" : "Message") : "Add a model in Settings to start"}
        spellCheck
        onChange={(e) => update(e.target.value)}
        onPaste={(e) => {
          const pasted = [...e.clipboardData.files];
          if (!pasted.length) return;
          e.preventDefault();
          void useStore.getState().attach({ files: pasted });
        }}
        onKeyDown={(e) => {
          // Shift+Tab: the next permission mode.
          if (agent && e.key === "Tab" && e.shiftKey && !e.ctrlKey && !e.altKey) {
            e.preventDefault();
            cycleMode();
            return;
          }
          if (e.key !== "Enter" || e.nativeEvent.isComposing) return;
          if (wantsSend(e, sendKey)) {
            e.preventDefault();
            void submit();
          }
        }}
      />
      {agent && <NoToolsNote />}
      <div className="composer-bar">
        <PlusMenu />
        {agent && <FolderChip />}
        <div className="composer-fill" />
        {agent && <ModeChip />}
        <ModelEffortPicker value={model} />
        <ContextMeter />
        {!!navigator.mediaDevices && <MicButton recording={recording} onStart={() => setRecording(true)} />}
        {streaming ? (
          <motion.button
            key="stop"
            className="send-btn stop"
            aria-label="Stop (Esc)"
            title="Stop (Esc)"
            whileTap={{ scale: 0.92 }}
            initial={{ scale: 0.6, opacity: 0 }}
            animate={{ scale: 1, opacity: 1 }}
            transition={spring}
            onClick={() => useStore.getState().stop()}
          >
            <SendGlyph kind="stop" />
          </motion.button>
        ) : (
          <motion.button
            key="send"
            className="send-btn"
            aria-label="Send"
            title={sendKey === "enter" ? "Send (Enter)" : "Send (Ctrl+Enter)"}
            disabled={!canSend}
            whileTap={{ scale: 0.92 }}
            initial={{ scale: 0.6, opacity: 0 }}
            animate={{ scale: 1, opacity: 1 }}
            transition={spring}
            onClick={() => void submit()}
          >
            <SendGlyph kind="send" />
          </motion.button>
        )}
      </div>
    </div>
  );
}

/** The send and stop buttons draw their disc and glyph in one SVG. Chrome
 *  snaps an icon to whole pixels but paints a background at the button's exact
 *  spot, so on a button at x.5 a separate icon sat half a pixel off; drawn
 *  together they can't drift apart. The arrow is lucide's ArrowUp at 17px, 1px
 *  below center: its weight is in the head, so centered it looks high. */
function SendGlyph({ kind }: { kind: "send" | "stop" }) {
  return (
    <svg width="32" height="32" viewBox="0 0 32 32" aria-hidden="true">
      <circle className="send-disc" cx="16" cy="16" r="16" />
      {kind === "send" ? (
        <g
          transform="translate(7.5 8.5) scale(0.708333)"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.4"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <path d="m5 12 7-7 7 7" />
          <path d="M12 19V5" />
        </g>
      ) : (
        <rect x="11" y="11" width="10" height="10" rx="1.5" fill="currentColor" />
      )}
    </svg>
  );
}

/** The recording in progress, shared by the strip and the mic button. */
let live: { rec: Recording; started: number; done: (keep: boolean) => void } | null = null;

function MicButton({ recording, onStart }: { recording: boolean; onStart: () => void }) {
  const [starting, setStarting] = useState(false);
  if (recording) {
    return (
      <motion.button
        className="bar-btn rec-stop"
        aria-label="Stop and add the voice message"
        title="Stop and add the voice message"
        initial={{ scale: 0.8 }}
        animate={{ scale: 1 }}
        transition={spring}
        onClick={() => live?.done(true)}
      >
        <Square size={11} fill="currentColor" />
      </motion.button>
    );
  }
  return (
    <button
      className="bar-btn"
      aria-label="Record a voice message"
      title="Record a voice message"
      disabled={starting}
      onClick={async () => {
        setStarting(true);
        try {
          const rec = await record();
          live = { rec, started: Date.now(), done: () => {} };
          onStart();
        } catch (e) {
          useStore.getState().toast(micError(e));
        } finally {
          setStarting(false);
        }
      }}
    >
      <Mic size={16} />
    </button>
  );
}

const BARS = 36;

/** While recording: the time, a level meter, and Cancel. */
function RecordingStrip({ onDone }: { onDone: () => void }) {
  const [levels, setLevels] = useState<number[]>(() => Array(BARS).fill(0));
  const [secs, setSecs] = useState(0);
  const finish = useCallback(
    async (keep: boolean) => {
      const cur = live;
      live = null;
      onDone();
      if (!cur) return;
      if (!keep) return cur.rec.cancel();
      try {
        const { wav, seconds } = await cur.rec.stop();
        if (seconds < 0.4) return useStore.getState().toast("That was too short to keep");
        await useStore.getState().attachVoice(wav, seconds);
      } catch (e) {
        useStore.getState().toast(e instanceof Error ? e.message : String(e));
      }
    },
    [onDone],
  );
  useEffect(() => {
    if (live) live.done = (keep) => void finish(keep);
    const tick = setInterval(() => {
      if (!live) return;
      const s = (Date.now() - live.started) / 1000;
      setSecs(s);
      setLevels((l) => [...l.slice(1), live!.rec.level()]);
      if (s >= MAX_RECORDING) void finish(true);
    }, 70);
    return () => clearInterval(tick);
  }, [finish]);
  // (Leaving the chat drops the recording: the composer's effect on its key.)
  useEscape(true, () => void finish(false));
  return (
    <motion.div
      className="rec"
      initial={{ opacity: 0, height: 0 }}
      animate={{ opacity: 1, height: 44 }}
      exit={{ opacity: 0, height: 0, transition: { duration: 0.12 } }}
      transition={spring}
    >
      <span className="rec-dot" />
      <span className="rec-time">{clock(secs)}</span>
      <div className="rec-levels" aria-hidden>
        {levels.map((v, i) => (
          <span key={i} style={{ height: 3 + Math.round(v * 19) }} />
        ))}
      </div>
      <button className="rec-cancel" onClick={() => void finish(false)}>
        Cancel
      </button>
    </motion.div>
  );
}

/** Enter (or Ctrl+Enter, per Settings) sends; the other inserts a line. */
export function wantsSend(e: React.KeyboardEvent, sendKey: string): boolean {
  if (sendKey === "enter") return !e.shiftKey;
  return e.ctrlKey;
}

/** The "+" menu: files, the preset, and the chat's switches (skills, MCP
 *  tools, web search). */
function PlusMenu() {
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const close = () => setAnchor(null);
  const opts = useChatOptions();
  const presets = useStore((s) => s.presets);
  const preset = presets.find((p) => p.id === opts.presetId);
  const set = useStore((s) => s.setOptions);
  return (
    <>
      <button
        ref={btn}
        className={`bar-btn ${anchor ? "open" : ""}`}
        aria-label="Add files, preset and tools"
        title="Add files, preset and tools"
        onClick={() => (anchor ? close() : setAnchor(anchorFrom(btn.current!, { above: true })))}
      >
        <Plus size={18} />
      </button>
      <Popover anchor={anchor} onClose={close} width={232} trigger={btn}>
        <div className="menu">
          <MenuItem
            icon={<Paperclip size={15} />}
            onSelect={async () => {
              close();
              const picked = await open({ multiple: true, title: "Add files" });
              if (!picked) return;
              void useStore.getState().attach({ paths: Array.isArray(picked) ? picked : [picked] });
            }}
          >
            Add files
          </MenuItem>
          <SubmenuItem icon={<SlidersHorizontal size={15} />} label="Preset" value={preset?.name ?? "None"} width={220}>
            {(done) => (
              <>
                <MenuItem
                  checked={!preset}
                  onSelect={() => {
                    done();
                    close();
                    set({ presetId: null });
                  }}
                >
                  None
                </MenuItem>
                {presets.map((p) => (
                  <MenuItem
                    key={p.id}
                    checked={p.id === preset?.id}
                    onSelect={() => {
                      done();
                      close();
                      set({ presetId: p.id });
                    }}
                  >
                    {p.name}
                  </MenuItem>
                ))}
                <MenuSeparator />
                <MenuItem
                  onSelect={() => {
                    done();
                    close();
                    useStore.getState().openSettings("presets", presets.length ? null : "new");
                  }}
                >
                  {presets.length ? "Manage presets…" : "Create a preset…"}
                </MenuItem>
              </>
            )}
          </SubmenuItem>
          <MenuSeparator />
          <SwitchItem icon={<BookMarked size={15} />} label="Skills" on={opts.skills} onChange={(skills) => set({ skills })} hint="The skills you turned on in Settings" />
          <SwitchItem icon={<Plug size={15} />} label="MCPs" on={opts.tools} onChange={(tools) => set({ tools })} hint="Tools from your MCP servers" />
          <SwitchItem icon={<Globe size={15} />} label="Web search" on={opts.web} onChange={(web) => set({ web })} hint="Search the web and read pages" />
        </div>
      </Popover>
    </>
  );
}

/** An Agent session's folder. Before the first message: pick one, or leave
 *  it empty and Relay makes a workspace. After: fixed, with Open in Explorer. */
function FolderChip() {
  const current = useStore((s) => s.current);
  const draftFolder = useStore((s) => s.draftOptions.folder ?? null);
  const folder = current ? current.folder : draftFolder;
  const info = useFolderInfo(folder);
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const close = () => setAnchor(null);

  const pick = async () => {
    const picked = await open({ directory: true, title: "Pick the session's folder" });
    if (typeof picked === "string") useStore.getState().setOptions({ folder: picked });
  };

  if (!current) {
    if (!folder) {
      return (
        <button className="chip folder-chip muted" title="Without one, Relay makes a workspace folder for this session" onClick={() => void pick()}>
          <FolderOpen size={15} className="chip-icon" />
          <span className="chip-label">Pick a folder</span>
        </button>
      );
    }
    return (
      <div className="folder-pick">
        <button className="chip folder-chip" title={`${folder}\nClick to pick another`} onClick={() => void pick()}>
          <FolderClosed size={15} className="chip-icon" />
          <span className="chip-label">{folderName(folder)}</span>
        </button>
        <button className="folder-clear" aria-label="No folder (Relay makes a workspace)" title="No folder (Relay makes a workspace)" onClick={() => useStore.getState().setOptions({ folder: null })}>
          <X size={13} />
        </button>
      </div>
    );
  }

  if (!folder) return null;
  const missing = info !== null && !info.exists;
  return (
    <>
      <button
        ref={btn}
        className={`chip folder-chip ${anchor ? "open" : ""} ${missing ? "missing" : ""}`}
        title={missing ? `${folder} no longer exists` : folder}
        onClick={() => (anchor ? close() : setAnchor(anchorFrom(btn.current!, { above: true })))}
      >
        <FolderClosed size={15} className="chip-icon" />
        <span className="chip-label">{current.folderOwned ? "Workspace" : folderName(folder)}</span>
      </button>
      <Popover anchor={anchor} onClose={close} width={280} trigger={btn}>
        <div className="menu">
          <div className="menu-note folder-path">{folder}</div>
          <MenuItem
            icon={<FolderOpen size={15} />}
            disabled={missing}
            onSelect={() => {
              close();
              void api.openSessionFolder(current.id).catch((e) => useStore.getState().toast(errorText(e)));
            }}
          >
            Open in Explorer
          </MenuItem>
          <MenuItem
            icon={<RefreshCw size={15} />}
            disabled={missing}
            onSelect={() => {
              close();
              void api
                .reloadInstructions(current.id)
                .then((n) => useStore.getState().toast(n ? "Instructions read again: the next message uses them" : "No AGENTS.md or CLAUDE.md in this folder"))
                .catch((e) => useStore.getState().toast(errorText(e)));
            }}
          >
            Reload instructions
          </MenuItem>
        </div>
      </Popover>
    </>
  );
}

/** Models whose tool calling nobody knows yet are asked once, quietly. */
const toolsAsked = new Set<string>();

/** In an Agent session: a model that can't call tools can't work in the folder. */
function NoToolsNote() {
  const model = useActiveModel();
  const key = model ? `${model.providerId}/${model.model}` : "";
  const c = useStore((s) => (key ? s.caps[key] : undefined));
  useEffect(() => {
    if (!model) return;
    useStore.getState().ensureCaps(model);
    if (c && c.caps.tools == null && !toolsAsked.has(key)) {
      toolsAsked.add(key);
      void useStore.getState().checkTools(model, true);
    }
  }, [model, key, c]);
  if (c?.caps.tools !== false) return null;
  return (
    <div className="composer-note">
      <CircleAlert size={13} />
      <span>{shortModel(model!.model)} can't call tools, so it can't read or change files here. Pick another model.</span>
    </div>
  );
}

/** The permission modes the chip offers, in Shift+Tab order. */
const MODES: { value: AgentMode; label: string; icon: LucideIcon; hint: string }[] = [
  { value: "ask", label: "Ask every time", icon: Hand, hint: "Every change and command waits for your OK" },
  { value: "edits", label: "Accept edits", icon: FilePen, hint: "File changes in the folder run; commands still ask" },
  { value: "bypass", label: "Bypass", icon: ShieldOff, hint: "Everything runs without asking, except reading credentials" },
];

function useMode(): AgentMode {
  return useStore((s) => (s.current ? s.current.mode : s.draftOptions.mode) ?? s.settings?.agentMode ?? "ask");
}

function cycleMode() {
  const st = useStore.getState();
  const now = (st.current ? st.current.mode : st.draftOptions.mode) ?? st.settings?.agentMode ?? "ask";
  const i = MODES.findIndex((m) => m.value === now);
  st.setOptions({ mode: MODES[(i + 1) % MODES.length].value });
}

/** An Agent session's permission mode, beside the model chip (Shift+Tab cycles). */
function ModeChip() {
  const mode = useMode();
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const close = () => setAnchor(null);
  const cur = MODES.find((m) => m.value === mode) ?? MODES[0];
  const Icon = cur.icon;
  return (
    <>
      <button
        ref={btn}
        className={`chip mode-chip mode-${mode} ${anchor ? "open" : ""}`}
        title={`${cur.hint} (Shift+Tab to change)`}
        onClick={() => (anchor ? close() : setAnchor(anchorFrom(btn.current!, { above: true, alignRight: true })))}
      >
        <Icon size={14} className="chip-icon" />
        <span className="chip-label">{cur.label}</span>
        <ChevronDown size={13} className="chip-caret" />
      </button>
      <Popover anchor={anchor} onClose={close} width={300} trigger={btn}>
        <div className="menu">
          {MODES.map((m) => (
            <MenuItem
              key={m.value}
              icon={<m.icon size={15} />}
              checked={m.value === mode}
              onSelect={() => {
                close();
                useStore.getState().setOptions({ mode: m.value });
              }}
            >
              <span className="menu-two">
                <span>{m.label}</span>
                <span className="menu-sub">{m.hint}</span>
              </span>
            </MenuItem>
          ))}
        </div>
      </Popover>
    </>
  );
}

/** How much of the model's context the conversation uses (from the last reply). */
function ContextMeter() {
  const model = useActiveModel();
  const current = useStore((s) => s.current);
  const limit = useStore((s) => (model ? s.contexts[`${model.providerId}/${model.model}`] : null));
  const last = current?.messages
    .slice()
    .reverse()
    .find((m) => m.role === "assistant" && m.stats?.promptTokens);
  const used = last ? (last.stats!.promptTokens ?? 0) + (last.stats!.completionTokens ?? 0) : 0;
  if (!used) return null;
  const pct = limit ? Math.min(1, used / limit) : null;
  const r = 7;
  const c = 2 * Math.PI * r;
  const tone = pct === null ? "" : pct > 0.9 ? "danger" : pct > 0.75 ? "warn" : "";
  return (
    <div className={`context ${tone}`} title={limit ? `${used.toLocaleString()} of ${limit.toLocaleString()} tokens of context` : `${used.toLocaleString()} tokens of context`}>
      {pct !== null && (
        <svg width="18" height="18" viewBox="0 0 18 18" aria-hidden>
          <circle cx="9" cy="9" r={r} className="context-track" />
          <motion.circle
            cx="9"
            cy="9"
            r={r}
            className="context-fill"
            strokeDasharray={c}
            initial={false}
            animate={{ strokeDashoffset: c * (1 - pct) }}
            transition={spring}
            transform="rotate(-90 9 9)"
          />
        </svg>
      )}
      <span className="context-text">{limit ? `${compact(used)} / ${compact(limit)}` : `${compact(used)} tokens`}</span>
    </div>
  );
}
