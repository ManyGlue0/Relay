import { motion } from "framer-motion";
import { ArrowUp, BookMarked, Brain, ChevronDown, Settings2, Square, Wrench, type LucideIcon } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { EFFORTS, effortOf } from "../lib/api";
import { spring } from "../lib/motion";
import { compact, useActiveModel, useChatOptions, useStore } from "../lib/store";
import { ModelPicker } from "./ModelPicker";
import { MenuItem, MenuLabel, MenuSeparator, Popover, Toggle, anchorFrom, type Anchor, Dot } from "./ui";

/** Drafts survive switching chats (per chat, for this session). */
const drafts = new Map<string, string>();

export function Composer({ chatId, streaming }: { chatId: string | null; streaming: boolean }) {
  const key = chatId ?? "new";
  const [text, setText] = useState(() => drafts.get(key) ?? "");
  const ref = useRef<HTMLTextAreaElement>(null);
  const model = useActiveModel();
  const sendKey = useStore((s) => s.settings?.sendKey ?? "enter");
  const focusTick = useStore((s) => s.focusComposer);
  const [sending, setSending] = useState(false);

  useEffect(() => {
    setText(drafts.get(key) ?? "");
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

  const canSend = !!text.trim() && !!model && !streaming && !sending;

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
      <textarea
        ref={ref}
        rows={1}
        value={text}
        placeholder={model ? "Message" : "Add a model in Settings to start"}
        spellCheck
        onChange={(e) => update(e.target.value)}
        onKeyDown={(e) => {
          if (e.key !== "Enter" || e.nativeEvent.isComposing) return;
          if (wantsSend(e, sendKey)) {
            e.preventDefault();
            void submit();
          }
        }}
      />
      <div className="composer-bar">
        <ModelPicker value={model} />
        <EffortChip />
        <PresetChip />
        <ToolsChip />
        <div className="composer-fill" />
        <ContextMeter />
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
            <Square size={12} fill="currentColor" />
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
            <ArrowUp size={17} strokeWidth={2.4} />
          </motion.button>
        )}
      </div>
    </div>
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
