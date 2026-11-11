import { AnimatePresence, motion } from "framer-motion";
import {
  BookMarked,
  Brain,
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  CircleAlert,
  CircleCheck,
  CircleSlash,
  Copy,
  FileText,
  Loader,
  Pencil,
  RefreshCw,
  ShieldQuestion,
  Square,
  SquareTerminal,
  Wrench,
} from "lucide-react";
import { memo, useEffect, useRef, useState } from "react";
import { partsOf, SKILLS_SERVER, type Message, type Part } from "../lib/api";
import { spring } from "../lib/motion";
import { shortModel, useActiveModel, useStore } from "../lib/store";
import { fullTime, seconds, sentAgo, useNow } from "../lib/time";
import { wantsSend } from "./Composer";
import { Markdown } from "./Markdown";
import { ModelMenu } from "./ModelPicker";
import { AutoText, Button, IconButton, Popover, anchorFrom, copyText, type Anchor } from "./ui";

export const MessageView = memo(function MessageView({ message, busy }: { message: Message; busy: boolean }) {
  return message.role === "user" ? <UserMessage message={message} busy={busy} /> : <AssistantMessage message={message} busy={busy} />;
});

function UserMessage({ message, busy }: { message: Message; busy: boolean }) {
  const [editing, setEditing] = useState(false);
  if (editing) return <EditBox message={message} onDone={() => setEditing(false)} />;
  return (
    <div className="msg msg-user">
      <div className="bubble" onDoubleClick={() => !busy && setEditing(true)}>
        {message.content}
      </div>
      <div className="msg-actions">
        <Branches message={message} disabled={busy} />
        <CopyButton text={message.content} />
        <IconButton size="tiny" label="Edit" disabled={busy} onClick={() => setEditing(true)}>
          <Pencil size={13} />
        </IconButton>
      </div>
    </div>
  );
}

function EditBox({ message, onDone }: { message: Message; onDone: () => void }) {
  const [text, setText] = useState(message.content);
  const sendKey = useStore((s) => s.settings?.sendKey ?? "enter");
  const [saving, setSaving] = useState(false);
  const submit = async () => {
    if (!text.trim() || saving) return;
    if (text.trim() === message.content.trim()) return onDone();
    setSaving(true);
    const ok = await useStore.getState().edit(message.id, text);
    setSaving(false);
    if (ok) onDone();
  };
  return (
    <motion.div className="msg msg-user editing" initial={{ opacity: 0.6, scale: 0.99 }} animate={{ opacity: 1, scale: 1 }} transition={spring}>
      <div className="edit-box">
        <AutoText
          value={text}
          onChange={setText}
          minRows={1}
          autoFocus
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              e.stopPropagation();
              onDone();
            } else if (e.key === "Enter" && !e.nativeEvent.isComposing && wantsSend(e, sendKey)) {
              e.preventDefault();
              void submit();
            }
          }}
        />
        <div className="edit-bar">
          <span className="edit-hint">Sending starts a new branch; the original stays.</span>
          <Button variant="ghost" onClick={onDone}>
            Cancel
          </Button>
          <Button variant="primary" onClick={() => void submit()} disabled={!text.trim() || saving}>
            Send
          </Button>
        </div>
      </div>
    </motion.div>
  );
}

function AssistantMessage({ message, busy }: { message: Message; busy: boolean }) {
  const streaming = message.status === "streaming";
  const parts = partsOf(message);
  const waiting = streaming && parts.length === 0;
  const showStats = useStore((s) => s.settings?.showStats ?? true);
  return (
    <div className={`msg msg-assistant ${streaming ? "streaming" : ""}`}>
      {parts.map((p, i) => {
        const last = i === parts.length - 1;
        if (p.type === "reasoning") return <Reasoning key={i} text={p.text} live={streaming && last && !!message.thinking} ms={p.ms ?? null} />;
        if (p.type === "text") return <Markdown key={i} text={p.text} streaming={streaming && last} />;
        return <ToolCard key={p.id} part={p} messageId={message.id} />;
      })}
      {waiting && <Waiting />}
      {message.status === "error" && (
        <div className="msg-error">
          <CircleAlert size={15} />
          <span>{message.error || "Something went wrong"}</span>
        </div>
      )}
      {message.status === "stopped" && (
        <div className="msg-note">
          <Square size={10} fill="currentColor" />
          <span>Stopped</span>
        </div>
      )}
      {!streaming && (
        <div className="msg-foot">
          {showStats && <Meta message={message} />}
          <div className="msg-actions">
            <Branches message={message} disabled={busy} />
            {message.content && <CopyButton text={message.content} />}
            <Regenerate message={message} disabled={busy} />
            <SentAgo ms={message.createdAt} />
          </div>
        </div>
      )}
    </div>
  );
}

/** "‹ 2 / 3 ›": switch between this message's alternatives. */
function Branches({ message, disabled }: { message: Message; disabled: boolean }) {
  const n = message.siblings.length;
  if (n < 2) return null;
  const i = message.siblings.indexOf(message.id);
  const go = (d: number) => {
    const target = message.siblings[i + d];
    if (target) void useStore.getState().branch(target);
  };
  return (
    <span className="branches">
      <IconButton size="tiny" label="Previous version" disabled={disabled || i <= 0} onClick={() => go(-1)}>
        <ChevronLeft size={14} />
      </IconButton>
      <span className="branches-count">
        {i + 1} / {n}
      </span>
      <IconButton size="tiny" label="Next version" disabled={disabled || i >= n - 1} onClick={() => go(1)}>
        <ChevronRight size={14} />
      </IconButton>
    </span>
  );
}

function Regenerate({ message, disabled }: { message: Message; disabled: boolean }) {
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const caret = useRef<HTMLButtonElement>(null);
  const model = useActiveModel();
  return (
    <span className="split-tiny">
      <IconButton size="tiny" label="Try again" disabled={disabled} onClick={() => void useStore.getState().regenerate(message.id)}>
        <RefreshCw size={13} />
      </IconButton>
      <IconButton
        ref={caret}
        size="tiny"
        label="Try again with another model"
        className="split-caret"
        disabled={disabled}
        onClick={() => setAnchor(anchor ? null : anchorFrom(caret.current!, { above: false }))}
      >
        <ChevronDown size={12} />
      </IconButton>
      <Popover anchor={anchor} onClose={() => setAnchor(null)} width={320} className="model-pop" trigger={caret}>
        <ModelMenu
          value={model}
          onClose={() => setAnchor(null)}
          onPick={(ref) => {
            setAnchor(null);
            void useStore.getState().regenerate(message.id, ref);
          }}
        />
      </Popover>
    </span>
  );
}

function Waiting() {
  return (
    <div className="waiting" aria-label="Waiting for the model">
      {[0, 1, 2].map((i) => (
        <motion.span
          key={i}
          animate={{ opacity: [0.25, 1, 0.25], scale: [0.85, 1, 0.85] }}
          transition={{ duration: 1.2, repeat: Infinity, delay: i * 0.16, ease: "easeInOut" }}
        />
      ))}
    </div>
  );
}

/** The model's reasoning: open while it thinks (if the user likes), folded after. */
function Reasoning({ text, live, ms }: { text: string; live: boolean; ms: number | null }) {
  const expandWhileLive = useStore((s) => s.settings?.expandThinking ?? true);
  const [open, setOpen] = useState(live && expandWhileLive);
  const [touched, setTouched] = useState(false);
  useEffect(() => {
    if (!touched) setOpen(live && expandWhileLive);
  }, [live, expandWhileLive, touched]);
  const label = live ? "Thinking" : ms ? `Thought for ${seconds(ms)}` : "Reasoning";
  return (
    <div className={`reasoning ${open ? "open" : ""}`}>
      <button
        className={`pill-head ${live ? "live" : ""}`}
        onClick={() => {
          setTouched(true);
          setOpen(!open);
        }}
      >
        <Brain size={14} />
        <span className="pill-label">{label}</span>
        <motion.span className="pill-chev" animate={{ rotate: open ? 90 : 0 }} transition={spring}>
          <ChevronRight size={14} />
        </motion.span>
      </button>
      <Fold open={open}>
        <div className="reasoning-text">
          <Markdown text={text} streaming={live} />
        </div>
      </Fold>
    </div>
  );
}

function Fold({ open, children }: { open: boolean; children: React.ReactNode }) {
  return (
    <AnimatePresence initial={false}>
      {open && (
        <motion.div
          className="fold"
          initial={{ height: 0, opacity: 0 }}
          animate={{ height: "auto", opacity: 1 }}
          exit={{ height: 0, opacity: 0 }}
          transition={{ height: { type: "spring", stiffness: 420, damping: 40 }, opacity: { duration: 0.18 } }}
        >
          {children}
        </motion.div>
      )}
    </AnimatePresence>
  );
}

function prettyJson(text: string): string {
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text;
  }
}

const TOOL_STATUS: Record<string, { label: string; icon: React.ReactNode }> = {
  approval: { label: "Waiting for you", icon: <ShieldQuestion size={14} /> },
  running: { label: "Running", icon: <Loader size={14} className="spin" /> },
  done: { label: "Done", icon: <CircleCheck size={14} /> },
  error: { label: "Failed", icon: <CircleAlert size={14} /> },
  denied: { label: "Declined", icon: <CircleSlash size={14} /> },
};

type ToolPart = Extract<Part, { type: "tool" }>;

/** How a tool call reads on its card. The built-in skill tools read as what
 *  they do ("pdf · Skill", "scripts/fill.py · pdf"); MCP tools as tool and server. */
function toolFace(part: ToolPart): { icon: React.ReactNode; name: string; server: string; ask: string } {
  if (part.serverId !== SKILLS_SERVER) return { icon: <Wrench size={13} />, name: part.tool, server: part.server, ask: "Let the model run this tool?" };
  let a: Record<string, unknown> = {};
  try {
    a = JSON.parse(part.arguments) as Record<string, unknown>;
  } catch {
    /* half-written arguments */
  }
  const str = (k: string) => (typeof a[k] === "string" ? (a[k] as string) : "");
  switch (part.tool) {
    case "load":
      return { icon: <BookMarked size={13} />, name: str("name") || "Skill", server: "Skill", ask: "" };
    case "read_file":
      return { icon: <FileText size={13} />, name: str("path") || "File", server: str("skill"), ask: "" };
    default:
      return { icon: <SquareTerminal size={13} />, name: str("path") || "Script", server: str("skill"), ask: "Let the model run this script?" };
  }
}

function ToolCard({ part, messageId }: { part: ToolPart; messageId: string }) {
  const [open, setOpen] = useState(false);
  const st = TOOL_STATUS[part.status] ?? TOOL_STATUS.running;
  const asking = part.status === "approval";
  const args = prettyJson(part.arguments);
  const face = toolFace(part);
  return (
    <motion.div className={`tool-card ${part.status}`} layout="position" transition={spring}>
      <button className="tool-head" onClick={() => setOpen(!open)}>
        <span className="tool-well">{face.icon}</span>
        <span className="tool-name">{face.name}</span>
        {face.server && <span className="tool-server">{face.server}</span>}
        <span className="tool-fill" />
        <span className="tool-status">
          {st.icon}
          <span>{st.label}</span>
          {part.ms != null && part.ms >= 10 && part.status !== "approval" && <span className="tool-ms">{seconds(part.ms)}</span>}
        </span>
        <motion.span className="pill-chev" animate={{ rotate: open ? 90 : 0 }} transition={spring}>
          <ChevronRight size={14} />
        </motion.span>
      </button>
      {asking && (
        <div className="tool-ask">
          <pre className="tool-args inline">{args}</pre>
          <div className="tool-ask-bar">
            <span className="tool-ask-text">{face.ask}</span>
            <Button variant="ghost" onClick={() => void useStore.getState().decideTool(messageId, part.id, "deny")}>
              Deny
            </Button>
            <Button variant="default" onClick={() => void useStore.getState().decideTool(messageId, part.id, "always")}>
              Always allow
            </Button>
            <Button variant="primary" onClick={() => void useStore.getState().decideTool(messageId, part.id, "once")}>
              Allow once
            </Button>
          </div>
        </div>
      )}
      <Fold open={open && !asking}>
        <div className="tool-body">
          <div className="tool-label">Arguments</div>
          <pre className="tool-args">{args}</pre>
          {part.result != null && (
            <>
              <div className="tool-label">Result</div>
              <pre className="tool-result">{part.result || "(empty)"}</pre>
            </>
          )}
        </div>
      </Fold>
    </motion.div>
  );
}

function CopyButton({ text }: { text: string }) {
  const [done, setDone] = useState(false);
  return (
    <IconButton
      size="tiny"
      label={done ? "Copied" : "Copy"}
      onClick={() =>
        void copyText(text).then(() => {
          setDone(true);
          setTimeout(() => setDone(false), 1400);
        })
      }
    >
      {done ? <Check size={14} strokeWidth={2.4} /> : <Copy size={13} />}
    </IconButton>
  );
}

/** "5 minutes ago", after the buttons; the full date on hover. */
function SentAgo({ ms }: { ms: number }) {
  const now = useNow();
  return (
    <span className="msg-time" title={fullTime(ms)}>
      {sentAgo(ms, now)}
    </span>
  );
}

function Meta({ message }: { message: Message }) {
  const s = message.stats;
  const parts: string[] = [];
  if (message.model) parts.push(shortModel(message.model));
  if (s?.tokensPerSec) parts.push(`${s.tokensPerSec.toFixed(s.tokensPerSec < 10 ? 1 : 0)} tok/s`);
  if (s?.completionTokens) parts.push(`${s.estimated ? "~" : ""}${s.completionTokens.toLocaleString()} tokens`);
  if (s?.ttftMs != null) parts.push(`${seconds(s.ttftMs)} to first token`);
  if (s?.toolCalls) parts.push(s.toolCalls === 1 ? "1 tool call" : `${s.toolCalls} tool calls`);
  if (s?.finishReason === "length") parts.push("cut off at the length limit");
  if (!parts.length) return null;
  return <span className="msg-meta">{parts.join(" · ")}</span>;
}
