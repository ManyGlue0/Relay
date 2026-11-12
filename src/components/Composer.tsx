import { open } from "@tauri-apps/plugin-dialog";
import { AnimatePresence, motion } from "framer-motion";
import { BookMarked, Brain, ChevronDown, Mic, Paperclip, Settings2, Square, Wrench, type LucideIcon } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { EFFORTS, effortOf } from "../lib/api";
import { clock, micError, record, type Recording } from "../lib/media";
import { spring } from "../lib/motion";
import { compact, useActiveModel, useChatOptions, useEscape, useStore, type DraftFile } from "../lib/store";
import { AttachmentTray } from "./Attachments";
import { ModelPicker } from "./ModelPicker";
import { MenuItem, MenuLabel, MenuSeparator, Popover, Toggle, anchorFrom, type Anchor, Dot } from "./ui";

/** Drafts survive switching chats (per chat, for this session). */
const drafts = new Map<string, string>();
const NO_FILES: DraftFile[] = [];
/** Voice messages stop by themselves after this long. */
const MAX_RECORDING = 10 * 60;

export function Composer({ chatId, streaming }: { chatId: string | null; streaming: boolean }) {
  const key = chatId ?? "new";
  const [text, setText] = useState(() => drafts.get(key) ?? "");
  const ref = useRef<HTMLTextAreaElement>(null);
  const model = useActiveModel();
  const sendKey = useStore((s) => s.settings?.sendKey ?? "enter");
  const focusTick = useStore((s) => s.focusComposer);
  const files = useStore((s) => s.files[key]) ?? NO_FILES;
  const [sending, setSending] = useState(false);
  const [recording, setRecording] = useState(false);

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
        placeholder={model ? (files.length ? "Add a message, or just send" : "Message") : "Add a model in Settings to start"}
        spellCheck
        onChange={(e) => update(e.target.value)}
        onPaste={(e) => {
          const pasted = [...e.clipboardData.files];
          if (!pasted.length) return;
          e.preventDefault();
          void useStore.getState().attach({ files: pasted });
        }}
        onKeyDown={(e) => {
          if (e.key !== "Enter" || e.nativeEvent.isComposing) return;
          if (wantsSend(e, sendKey)) {
            e.preventDefault();
            void submit();
          }
        }}
      />
      <div className="composer-bar">
        <AttachButton />
        <ModelPicker value={model} />
        <EffortChip />
        <PresetChip />
        <ToolsChip />
        <div className="composer-fill" />
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

function AttachButton() {
  return (
    <button
      className="bar-btn"
      aria-label="Attach files"
      title="Attach files (or drop or paste them)"
      onClick={async () => {
        const picked = await open({ multiple: true, title: "Attach files" });
        if (!picked) return;
        void useStore.getState().attach({ paths: Array.isArray(picked) ? picked : [picked] });
      }}
    >
      <Paperclip size={16} />
    </button>
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

/** A chip in the composer bar that opens a small menu. */
function Chip({
  icon: Icon,
  label,
  title,
  muted,
  children,
  width = 260,
}: {
  icon: LucideIcon;
  label: string;
  title: string;
  muted?: boolean;
  children: (close: () => void) => React.ReactNode;
  width?: number;
}) {
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const close = () => setAnchor(null);
  return (
    <>
      <button ref={btn} className={`chip ${anchor ? "open" : ""} ${muted ? "muted" : ""}`} title={title} onClick={() => (anchor ? close() : setAnchor(anchorFrom(btn.current!, { above: true })))}>
        <Icon size={14} className="chip-icon" />
        <span className="chip-label">{label}</span>
        <ChevronDown size={13} className="chip-caret" />
      </button>
      <Popover anchor={anchor} onClose={close} width={width} trigger={btn}>
        <div className="menu">{children(close)}</div>
      </Popover>
    </>
  );
}

function EffortChip() {
  const opts = useChatOptions();
  const defaultEffort = useStore((s) => s.settings?.defaultEffort);
  const presets = useStore((s) => s.presets);
  const presetEffort = presets.find((p) => p.id === opts.presetId)?.effort ?? null;
  const effective = effortOf(opts.effort ?? presetEffort ?? defaultEffort);
  const label = EFFORTS.find((e) => e.value === effective)?.label ?? "Medium";
  return (
    <Chip icon={Brain} label={label} title="How hard the model thinks">
      {(close) => (
        <>
          <MenuLabel>Thinking effort</MenuLabel>
          {EFFORTS.map((e) => (
            <MenuItem
              key={e.value}
              checked={effective === e.value}
              onSelect={() => {
                close();
                useStore.getState().setOptions({ effort: e.value });
              }}
            >
              <span className="menu-two">
                <span>{e.label}</span>
                <span className="menu-sub">{e.hint}</span>
              </span>
            </MenuItem>
          ))}
          <MenuSeparator />
          <div className="menu-note">Servers that can't change it simply ignore it.</div>
        </>
      )}
    </Chip>
  );
}

function PresetChip() {
  const opts = useChatOptions();
  const presets = useStore((s) => s.presets);
  const preset = presets.find((p) => p.id === opts.presetId);
  return (
    <Chip icon={Settings2} label={preset?.name ?? "Preset"} title="The system prompt and settings for this chat" muted={!preset}>
      {(close) => (
        <>
          <MenuLabel>Preset</MenuLabel>
          <MenuItem
            checked={!preset}
            onSelect={() => {
              close();
              useStore.getState().setOptions({ presetId: null });
            }}
          >
            None
          </MenuItem>
          {presets.map((p) => (
            <MenuItem
              key={p.id}
              checked={p.id === preset?.id}
              onSelect={() => {
                close();
                useStore.getState().setOptions({ presetId: p.id });
              }}
            >
              {p.name}
            </MenuItem>
          ))}
          <MenuSeparator />
          <MenuItem
            onSelect={() => {
              close();
              useStore.getState().openSettings("presets", presets.length ? null : "new");
            }}
          >
            {presets.length ? "Manage presets…" : "Create a preset…"}
          </MenuItem>
        </>
      )}
    </Chip>
  );
}

function ToolsChip() {
  const opts = useChatOptions();
  const servers = useStore((s) => s.mcp);
  const allSkills = useStore((s) => s.skills);
  if (!servers.length && !allSkills.length) return null;
  const skills = allSkills.filter((s) => s.enabled && !s.error).length;
  const ready = servers.filter((s) => s.enabled && s.status.state === "ready");
  const count = ready.reduce((n, s) => n + s.status.tools.filter((t) => (s.toolPolicy[t.name] ?? s.defaultPolicy) !== "off").length, 0);
  const plural = (n: number, one: string) => `${n} ${one}${n === 1 ? "" : "s"}`;
  const label = !opts.tools ? "Tools off" : [count ? plural(count, "tool") : "", skills ? plural(skills, "skill") : ""].filter(Boolean).join(" · ") || "Tools";
  return (
    <Chip icon={Wrench} label={label} title="Tools and skills the model may use" muted={!opts.tools || (!count && !skills)} width={280}>
      {(close) => (
        <>
          <div className="menu-toggle-row">
            <span>Use tools in this chat</span>
            <Toggle label="Use tools in this chat" on={opts.tools} onChange={(tools) => useStore.getState().setOptions({ tools })} />
          </div>
          {allSkills.length > 0 && (
            <>
              <MenuSeparator />
              <MenuItem
                icon={<BookMarked size={15} />}
                trailing={skills ? `${skills} on` : "All off"}
                onSelect={() => {
                  close();
                  useStore.getState().openSettings("skills");
                }}
              >
                Skills
              </MenuItem>
            </>
          )}
          {servers.length > 0 && <MenuSeparator />}
          {servers.length > 0 && <MenuLabel>Servers</MenuLabel>}
          {servers.map((s) => (
            <div key={s.id} className="menu-server">
              <Dot state={!s.enabled ? "off" : s.status.state === "ready" ? "ok" : s.status.state === "connecting" ? "busy" : s.status.state === "error" ? "error" : "off"} />
              <span className="menu-server-name">{s.name}</span>
              <span className="menu-sub">{!s.enabled ? "Off" : s.status.state === "ready" ? `${s.status.tools.length} tools` : s.status.state === "connecting" ? "Connecting" : "Not connected"}</span>
            </div>
          ))}
          <MenuSeparator />
          <MenuItem
            onSelect={() => {
              close();
              useStore.getState().openSettings("tools");
            }}
          >
            Manage tools…
          </MenuItem>
        </>
      )}
    </Chip>
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
