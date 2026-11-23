import { motion } from "framer-motion";
import {
  Activity,
  Check,
  ChevronRight,
  Circle,
  CircleDot,
  ListTodo,
  MessageCircleQuestion,
  CircleAlert,
  CircleCheck,
  CircleSlash,
  FilePen,
  FilePlus2,
  FileText,
  FolderTree,
  Loader,
  OctagonX,
  RotateCcw,
  Search,
  ShieldQuestion,
  SquareTerminal,
  Trash2,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { api, errorText, type Answer, type DiffRow, type Part, type Question, type ToolDecision, type ToolDetail } from "../lib/api";
import { spring } from "../lib/motion";
import { useStore } from "../lib/store";
import { seconds } from "../lib/time";
import { Markdown } from "./Markdown";
import { Button } from "./ui";

type ToolPart = Extract<Part, { type: "tool" }>;

const STATUS: Record<string, { label: string; icon: React.ReactNode }> = {
  approval: { label: "Waiting for you", icon: <ShieldQuestion size={14} /> },
  running: { label: "Running", icon: <Loader size={14} className="spin" /> },
  done: { label: "Done", icon: <CircleCheck size={14} /> },
  error: { label: "Failed", icon: <CircleAlert size={14} /> },
  denied: { label: "Declined", icon: <CircleSlash size={14} /> },
  blocked: { label: "Blocked", icon: <OctagonX size={14} /> },
};

function argsOf(part: ToolPart): Record<string, unknown> {
  try {
    return JSON.parse(part.arguments) as Record<string, unknown>;
  } catch {
    return {};
  }
}

/** What an agent tool call reads as: an icon, its subject, a verb, and the question its approval asks. */
function face(part: ToolPart, detail: ToolDetail) {
  const a = argsOf(part);
  const str = (k: string) => (typeof a[k] === "string" ? (a[k] as string) : "");
  const path = detail.path || str("path") || ".";
  switch (part.tool) {
    case "read_file":
      return { icon: <FileText size={13} />, name: path, verb: "Read", ask: "Read this file?" };
    case "write_file":
      return { icon: <FilePlus2 size={13} />, name: path, verb: detail.created === false ? "Write" : "Create", ask: detail.created === false ? "Replace this file?" : "Create this file?" };
    case "edit_file":
      return { icon: <FilePen size={13} />, name: path, verb: "Edit", ask: "Make this change?" };
    case "delete_path":
      return {
        icon: <Trash2 size={13} />,
        name: path,
        verb: "Delete",
        ask: detail.folder ? `Delete this folder${detail.files ? ` and its ${detail.files} ${detail.files === 1 ? "file" : "files"}` : ""}?` : "Delete this file?",
      };
    case "list":
      return { icon: <FolderTree size={13} />, name: str("glob") || path, verb: "List", ask: "List this folder?" };
    case "search":
      return { icon: <Search size={13} />, name: str("pattern"), verb: "Search", ask: "Search here?" };
    case "powershell":
    case "bash":
    case "cmd":
      return {
        icon: <SquareTerminal size={13} />,
        name: str("command").split("\n")[0],
        verb: part.tool === "powershell" ? "PowerShell" : part.tool === "bash" ? "Bash" : "cmd",
        ask: a.background ? "Run this in the background?" : "Run this command?",
      };
    case "processes":
      return { icon: <Activity size={13} />, name: [str("action") || "list", a.id != null ? String(a.id) : ""].filter(Boolean).join(" "), verb: "Processes", ask: "" };
    default:
      return { icon: <FileText size={13} />, name: part.tool, verb: "Agent", ask: "Let the model do this?" };
  }
}

/** Steps whose approval can switch the session to Accept edits. */
const EDITS = new Set(["write_file", "edit_file", "delete_path"]);

const SHELLS = new Set(["powershell", "bash", "cmd"]);

export function AgentToolCard({ part, messageId, stale }: { part: ToolPart; messageId: string; stale?: boolean }) {
  if (part.tool === "ask_user") return <QuestionCard part={part} messageId={messageId} />;
  if (part.tool === "todo") return <TodoCard part={part} stale={!!stale} />;
  if (part.tool === "summarize") return <SummaryDivider part={part} />;
  return <StepCard part={part} messageId={messageId} />;
}

/** Where a long session was summarized to save context: everything above
 *  stays on screen, the model goes on from the summary (open to read it). */
function SummaryDivider({ part }: { part: ToolPart }) {
  const [open, setOpen] = useState(false);
  const summary = part.detail?.summary;
  if (part.status === "running") {
    return (
      <div className="summary-divider">
        <span className="summary-rule" />
        <span className="summary-label">
          <Loader size={13} className="spin" />
          Summarizing the session to save context…
        </span>
        <span className="summary-rule" />
      </div>
    );
  }
  if (part.status !== "done" || !summary) {
    return (
      <div className="summary-divider failed" title={part.result ?? ""}>
        <span className="summary-rule" />
        <span className="summary-label">
          <CircleAlert size={13} />
          The session couldn't be summarized
        </span>
        <span className="summary-rule" />
      </div>
    );
  }
  return (
    <div className="summary-block">
      <button className="summary-divider" aria-expanded={open} title={open ? "Hide the summary" : "Read the summary the model goes on from"} onClick={() => setOpen(!open)}>
        <span className="summary-rule" />
        <span className="summary-label">
          Summarized here
          <ChevronRight size={13} className={`summary-caret ${open ? "open" : ""}`} />
        </span>
        <span className="summary-rule" />
      </button>
      {open && (
        <div className="summary-text">
          <Markdown text={summary} />
        </div>
      )}
    </div>
  );
}

function StepCard({ part, messageId }: { part: ToolPart; messageId: string }) {
  const [open, setOpen] = useState(false);
  const detail: ToolDetail = part.detail ?? {};
  const f = face(part, detail);
  const command = SHELLS.has(part.tool);
  let st = STATUS[part.status] ?? STATUS.running;
  // Commands say how they ended.
  if (command && part.status === "done" && detail.process != null) st = { label: `In the background · ${detail.process}`, icon: <Activity size={14} /> };
  else if (command && (part.status === "done" || part.status === "error") && detail.timedOut) st = { label: "Timed out", icon: <CircleAlert size={14} /> };
  else if (command && part.status === "error" && detail.exit != null) st = { label: `Exit ${detail.exit}`, icon: <CircleAlert size={14} /> };
  const asking = part.status === "approval";
  const diff = detail.diff;
  const showDiff = !!diff && (asking || part.status === "done") && diff.hunks.length > 0;
  const decide = (d: ToolDecision) => void useStore.getState().decideTool(messageId, part.id, d);
  return (
    <div className={`tool-card agent-card ${part.status}`}>
      <button className="tool-head" onClick={() => setOpen(!open)}>
        <span className="tool-well">{f.icon}</span>
        <span className="tool-verb">{f.verb}</span>
        <span className="tool-name">{f.name}</span>
        {diff && (diff.added > 0 || diff.removed > 0) && (
          <span className="diff-stat">
            {diff.added > 0 && <span className="add">+{diff.added}</span>}
            {diff.removed > 0 && <span className="del">−{diff.removed}</span>}
          </span>
        )}
        <span className="tool-fill" />
        <span className="tool-status">
          {st.icon}
          <span>{st.label}</span>
          {part.ms != null && part.ms >= 10 && !asking && <span className="tool-ms">{seconds(part.ms)}</span>}
        </span>
        <motion.span className="pill-chev" animate={{ rotate: open ? 90 : 0 }} transition={spring}>
          <ChevronRight size={14} />
        </motion.span>
      </button>
      {showDiff && <DiffView rows={diff.hunks} truncated={diff.truncated} />}
      {command && asking && <pre className="cmd-line">{(() => { try { return (JSON.parse(part.arguments) as { command?: string }).command ?? part.arguments; } catch { return part.arguments; } })()}</pre>}
      {command && !asking && (open || part.status === "running") && <Terminal text={detail.output ?? ""} live={part.status === "running"} />}
      {part.status === "blocked" && detail.blocked && (
        <div className="blocked-why">
          <strong>{detail.blocked.rule}</strong>: {detail.blocked.why}
        </div>
      )}
      {asking && (
        <div className="tool-ask">
          {detail.note && (
            <div className="tool-note">
              <CircleAlert size={13} />
              <span>{detail.note}</span>
            </div>
          )}
          <div className="tool-ask-bar">
            <span className="tool-ask-text">{f.ask}</span>
            <Button variant="ghost" onClick={() => decide("deny")}>
              Deny
            </Button>
            {EDITS.has(part.tool) && !detail.note && !command && (
              <Button variant="default" onClick={() => decide("edits")} title="Switch this session to Accept edits: file changes in the folder stop asking">
                Accept edits from now
              </Button>
            )}
            <Button variant="primary" onClick={() => decide("once")}>
              Allow
            </Button>
          </div>
        </div>
      )}
      {detail.checkpoint != null && !asking && part.status !== "running" && part.status !== "denied" && <RestoreBar checkpoint={detail.checkpoint} />}
      {detail.unprotected && !asking && <div className="tool-unprotected">Not undoable: {detail.unprotected}</div>}
      {open && !asking && !command && (
        <div className="tool-body">
          {part.result != null && <pre className="tool-result">{part.result || "(empty)"}</pre>}
          {part.result == null && <pre className="tool-args">{part.arguments}</pre>}
        </div>
      )}
    </div>
  );
}

/** The model's questions: pick options (or write your own), then send. */
function QuestionCard({ part, messageId }: { part: ToolPart; messageId: string }) {
  const qs: Question[] = part.detail?.questions ?? [];
  const asking = part.status === "question";
  const [answers, setAnswers] = useState<Answer[]>(() => qs.map(() => ({ picked: [], other: "" })));
  const [sending, setSending] = useState(false);
  const given = part.detail?.answers;
  const send = async (value: Answer[] | null) => {
    setSending(true);
    try {
      await api.answerTool(messageId, part.id, value);
    } catch (e) {
      useStore.getState().toast(errorText(e));
    } finally {
      setSending(false);
    }
  };
  const toggle = (i: number, label: string, multiple: boolean) =>
    setAnswers((all) =>
      all.map((a, j) => {
        if (j !== i) return a;
        const on = a.picked.includes(label);
        if (multiple) return { ...a, picked: on ? a.picked.filter((x) => x !== label) : [...a.picked, label] };
        return { ...a, picked: on ? [] : [label] };
      }),
    );
  const ready = answers.some((a) => a.picked.length > 0 || a.other.trim());
  return (
    <div className={`tool-card question-card ${asking ? "approval" : ""}`}>
      <div className="question-head">
        <span className="tool-well">
          <MessageCircleQuestion size={13} />
        </span>
        <span className="tool-verb">{asking ? (qs.length === 1 ? "A question for you" : `${qs.length} questions for you`) : "Asked you"}</span>
      </div>
      {qs.map((q, i) => (
        <div key={i} className="question">
          <div className="question-text">
            {q.header && <span className="question-tag">{q.header}</span>}
            {q.question}
          </div>
          {asking ? (
            <>
              <div className="question-options">
                {q.options.map((o) => {
                  const on = answers[i]?.picked.includes(o.label);
                  return (
                    <button key={o.label} className={`question-option ${on ? "on" : ""}`} onClick={() => toggle(i, o.label, q.multiple)} aria-pressed={on}>
                      <span className={`question-mark ${q.multiple ? "qm-box" : "qm-dot"}`}>{on && <Check size={11} strokeWidth={3} />}</span>
                      <span className="question-option-text">
                        <span className="question-label">{o.label}</span>
                        {o.description && <span className="question-desc">{o.description}</span>}
                      </span>
                    </button>
                  );
                })}
              </div>
              <input
                className="question-other"
                placeholder="Other: write your own answer"
                value={answers[i]?.other ?? ""}
                onChange={(e) => setAnswers((all) => all.map((a, j) => (j === i ? { ...a, other: e.target.value } : a)))}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && ready && !sending) void send(answers);
                }}
              />
            </>
          ) : (
            <div className="question-answer">{answerText(given?.[i]) ?? (given === null ? "Not answered" : part.status === "denied" ? "Stopped" : "…")}</div>
          )}
        </div>
      ))}
      {asking && (
        <div className="tool-ask-bar question-bar">
          <span className="tool-ask-text">{qs.some((q) => q.multiple) ? "Pick one or more, or write your own" : "Pick one, or write your own"}</span>
          <Button variant="ghost" disabled={sending} onClick={() => void send(null)}>
            Skip
          </Button>
          <Button variant="primary" disabled={!ready || sending} onClick={() => void send(answers)}>
            Send answers
          </Button>
        </div>
      )}
    </div>
  );
}

function answerText(a: Answer | undefined): string | null {
  if (!a) return null;
  const parts = [...a.picked, ...(a.other.trim() ? [`“${a.other.trim()}”`] : [])];
  return parts.length ? parts.join(", ") : "No answer";
}

/** The model's to-do list. Earlier versions in the same reply fold to one line. */
function TodoCard({ part, stale }: { part: ToolPart; stale: boolean }) {
  const items = part.detail?.items ?? [];
  const done = items.filter((i) => i.status === "done").length;
  if (stale) {
    return (
      <div className="todo-line">
        <ListTodo size={13} />
        <span>
          Updated the to-do list · {done}/{items.length}
        </span>
      </div>
    );
  }
  return (
    <div className="tool-card todo-card">
      <div className="question-head">
        <span className="tool-well">
          <ListTodo size={13} />
        </span>
        <span className="tool-verb">To-do</span>
        <span className="tool-fill" />
        <span className="todo-count">
          {done}/{items.length}
        </span>
      </div>
      <ul className="todo-list">
        {items.map((it, i) => (
          <li key={i} className={`todo-item ${it.status}`}>
            {it.status === "done" ? <CircleCheck size={14} /> : it.status === "in_progress" ? <CircleDot size={14} /> : <Circle size={14} />}
            <span>{it.text}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

/** A command's output, terminal style; follows the end while it runs. */
function Terminal({ text, live }: { text: string; live: boolean }) {
  const ref = useRef<HTMLPreElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (el && live) el.scrollTop = el.scrollHeight;
  }, [text, live]);
  return (
    <pre ref={ref} className="terminal">
      {text || (live ? "Waiting for output…" : "No output.")}
    </pre>
  );
}

/** "Restore to before this": the folder as it was right before the step. */
function RestoreBar({ checkpoint }: { checkpoint: number }) {
  const gone = useStore((s) => !!s.collapsed[checkpoint]);
  const busy = useStore((s) => (s.currentId ? !!s.streaming[s.currentId] : false));
  if (gone) return <div className="tool-restore gone">No longer restorable (pruned to save space)</div>;
  return (
    <div className="tool-restore">
      <button disabled={busy} title={busy ? "Wait for the reply to finish" : "Put the folder back as it was right before this step (you can undo it)"} onClick={() => void useStore.getState().restoreTo(checkpoint)}>
        <RotateCcw size={12} />
        Restore to before this
      </button>
    </div>
  );
}

/** Rows a diff shows before "Show all". */
const FOLDED = 12;

export function DiffView({ rows, truncated }: { rows: DiffRow[][]; truncated?: boolean }) {
  const [all, setAll] = useState(false);
  const total = rows.reduce((n, h) => n + h.length, 0);
  let left = all ? Infinity : FOLDED;
  const shown: DiffRow[][] = [];
  for (const h of rows) {
    if (left <= 0) break;
    shown.push(h.slice(0, left));
    left -= h.length;
  }
  return (
    <div className="diff">
      <div className="diff-scroll">
        {shown.map((h, i) => (
          <div key={i} className="diff-hunk">
            {i > 0 && <div className="diff-gap">⋯</div>}
            {h.map(([kind, a, b, text], j) => (
              <div key={j} className={`diff-row ${kind === "+" ? "add" : kind === "-" ? "del" : ""}`}>
                <span className="diff-no">{a ?? ""}</span>
                <span className="diff-no">{b ?? ""}</span>
                <span className="diff-sign">{kind === " " ? "" : kind === "-" ? "−" : "+"}</span>
                <span className="diff-text">{text || " "}</span>
              </div>
            ))}
          </div>
        ))}
      </div>
      {total > FOLDED && (
        <button className="diff-more" onClick={() => setAll(!all)}>
          {all ? "Show less" : `Show all ${total} lines`}
        </button>
      )}
      {all && truncated && <div className="diff-cut">The rest of this change is too long to show here.</div>}
    </div>
  );
}
