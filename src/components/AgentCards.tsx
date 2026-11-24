import { AnimatePresence, motion } from "framer-motion";
import {
  Activity,
  ChevronRight,
  Circle,
  CircleDot,
  ListTodo,
  Minus,
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
import { type Answer, type DiffRow, type Part, type Question, type ToolDecision, type ToolDetail } from "../lib/api";
import { spring } from "../lib/motion";
import { useStore } from "../lib/store";
import { useShallow } from "zustand/react/shallow";
import { seconds } from "../lib/time";
import { Markdown } from "./Markdown";
import { Button, IconButton } from "./ui";

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
    case "processes": {
      const id = a.id != null ? String(a.id) : "";
      const name = { wait: `Wait for process ${id}`, read: `Read process ${id}'s output`, stop: `Stop process ${id}` }[str("action")] ?? "List the background processes";
      return { icon: <Activity size={13} />, name, verb: "Processes", ask: "", plain: true };
    }
    default:
      return { icon: <FileText size={13} />, name: part.tool, verb: "Agent", ask: "Let the model do this?" };
  }
}

/** Steps whose approval can switch the session to Accept edits. */
const EDITS = new Set(["write_file", "edit_file", "delete_path"]);

const SHELLS = new Set(["powershell", "bash", "cmd"]);

export function AgentToolCard({ part, messageId }: { part: ToolPart; messageId: string }) {
  if (part.tool === "ask_user") return <QuestionCard part={part} />;
  if (part.tool === "todo") return <TodoCard part={part} />;
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
  const args = argsOf(part);
  // The model's own words for the step, for people who don't read code.
  const explained = typeof args.description === "string" ? args.description.trim() : "";
  const command = SHELLS.has(part.tool);
  const commandText = command ? (typeof args.command === "string" ? args.command : part.arguments) : "";
  let st = STATUS[part.status] ?? STATUS.running;
  // Commands say how they ended.
  if (command && part.status === "done" && detail.process != null) st = { label: `In the background · ${detail.process}`, icon: <Activity size={14} /> };
  else if (command && (part.status === "done" || part.status === "error") && detail.timedOut) st = { label: "Timed out", icon: <CircleAlert size={14} /> };
  else if (command && part.status === "error" && detail.exit != null) st = { label: `Exit ${detail.exit}`, icon: <CircleAlert size={14} /> };
  const asking = part.status === "approval";
  const diff = detail.diff;
  const showDiff = !!diff && (asking || part.status === "done") && diff.hunks.length > 0;
  const restorable = detail.checkpoint != null && part.status !== "running" && part.status !== "denied";
  const decide = (d: ToolDecision) => void useStore.getState().decideTool(messageId, part.id, d);
  const head = (
    <>
      <span className="tool-well">{f.icon}</span>
      {f.plain && !explained ? (
        <span className="tool-title">{f.name}</span>
      ) : explained ? (
        <>
          <span className="tool-title" title={explained}>
            {explained}
          </span>
          {!command && <span className="tool-sub">{f.name}</span>}
        </>
      ) : (
        <>
          <span className="tool-verb">{f.verb}</span>
          <span className="tool-name">{f.name}</span>
        </>
      )}
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
    </>
  );
  return (
    <div className={`tool-card agent-card ${part.status}`}>
      {asking ? (
        // Nothing to open while it waits: everything it asks about is showing.
        <div className="tool-head still">{head}</div>
      ) : (
        <button className="tool-head" onClick={() => setOpen(!open)} aria-expanded={open}>
          {head}
          <motion.span className="pill-chev" animate={{ rotate: open ? 90 : 0 }} transition={spring}>
            <ChevronRight size={14} />
          </motion.span>
        </button>
      )}
      {command && (asking || open) && <CommandLine shell={f.verb} text={commandText} />}
      {showDiff && <DiffView rows={diff.hunks} truncated={diff.truncated} />}
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
      {open && !asking && !command && (
        <div className="tool-body">
          {explained && <div className="tool-label">{`${f.verb} ${f.name}`}</div>}
          {part.result != null && <pre className="tool-result">{part.result || "(empty)"}</pre>}
          {part.result == null && <pre className="tool-args">{part.arguments}</pre>}
        </div>
      )}
      {open && !asking && detail.unprotected && <div className="tool-unprotected">Not undoable: {detail.unprotected}</div>}
      {open && !asking && restorable && <RestoreBar checkpoint={detail.checkpoint!} />}
    </div>
  );
}

/** The command a step runs, whole: in approvals and opened cards. */
function CommandLine({ shell, text }: { shell: string; text: string }) {
  return (
    <div className="cmd-block">
      <div className="cmd-shell">{shell}</div>
      <pre className="cmd-line">{text}</pre>
    </div>
  );
}

/** The model's questions in the conversation: while they wait, a line (you
 *  answer in the dock above the composer, `QuestionDock`); after, what it
 *  asked and what you said. */
function QuestionCard({ part }: { part: ToolPart }) {
  const qs: Question[] = part.detail?.questions ?? [];
  const given = part.detail?.answers;
  if (part.status === "question") {
    return (
      <div className="question-line asking">
        <MessageCircleQuestion size={13} />
        <span>{qs.length === 1 ? "Asked you a question: answer it above the message box" : `Asked you ${qs.length} questions: answer them above the message box`}</span>
      </div>
    );
  }
  return (
    <div className="tool-card question-card">
      <div className="question-head">
        <span className="tool-well">
          <MessageCircleQuestion size={14} />
        </span>
        <span className="tool-verb">Asked you</span>
      </div>
      {qs.map((q, i) => (
        <div key={i} className="question">
          <div className="question-text">
            {q.header && <span className="question-tag">{q.header}</span>}
            {q.question}
          </div>
          <div className="question-answer">{answerText(given?.[i]) ?? (given === null ? "Skipped" : part.status === "denied" ? "Stopped" : "…")}</div>
        </div>
      ))}
    </div>
  );
}

function answerText(a: Answer | undefined): string | null {
  if (!a) return null;
  const parts = [...a.picked, ...(a.other.trim() ? [`“${a.other.trim()}”`] : [])];
  return parts.length ? parts.join(", ") : "No answer";
}

/** The model's to-do list, in the conversation: one line. The list itself
 *  floats over the chat (`TodoFloat`), where it's always in sight. */
function TodoCard({ part }: { part: ToolPart }) {
  const items = part.detail?.items ?? [];
  const done = items.filter((i) => i.status === "done").length;
  return (
    <div className="todo-line">
      <ListTodo size={13} />
      <span>{items.length ? `Updated the to-do list · ${done}/${items.length}` : "Cleared the to-do list"}</span>
    </div>
  );
}

type TodoItem = NonNullable<ToolDetail["items"]>[number];

/** The newest to-do list on the conversation's branch (null: none, or emptied). */
function useTodo(): TodoItem[] | null {
  return useStore(
    useShallow((s) => {
      const msgs = s.current && s.current.id === s.currentId ? s.current.messages : [];
      for (let m = msgs.length - 1; m >= 0; m--) {
        const parts = msgs[m].parts ?? [];
        for (let i = parts.length - 1; i >= 0; i--) {
          const p = parts[i];
          if (p.type === "tool" && p.tool === "todo" && p.detail?.items) return p.detail.items.length ? p.detail.items : null;
        }
      }
      return null;
    }),
  );
}

/** The to-do list, floating at the chat's top right; folds to a chip. */
export function TodoFloat() {
  const items = useTodo();
  const chatId = useStore((s) => s.currentId);
  const choice = useStore((s) => (chatId ? s.todoFolded[chatId] : undefined));
  if (!items || !chatId) return null;
  const done = items.filter((i) => i.status === "done").length;
  // Unless you say otherwise, it folds away once everything is done.
  const folded = choice ?? done === items.length;
  const fold = (on: boolean) => useStore.setState((s) => ({ todoFolded: { ...s.todoFolded, [chatId]: on } }));
  return (
    <div className="todo-float-wrap">
      <AnimatePresence initial={false} mode="popLayout">
        {folded ? (
          <motion.button
            key="chip"
            className="todo-chip"
            title="Show the to-do list"
            onClick={() => fold(false)}
            initial={{ opacity: 0, scale: 0.92 }}
            animate={{ opacity: 1, scale: 1 }}
            exit={{ opacity: 0, scale: 0.92 }}
            transition={spring}
          >
            <ListTodo size={14} />
            <span className="todo-count">
              {done}/{items.length}
            </span>
          </motion.button>
        ) : (
          <motion.div
            key="card"
            className="todo-float"
            initial={{ opacity: 0, scale: 0.96, y: -4 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: 0.96, y: -4 }}
            transition={spring}
          >
            <div className="todo-float-head">
              <ListTodo size={14} className="todo-float-icon" />
              <span className="todo-float-title">To-do</span>
              <span className="todo-count">
                {done}/{items.length}
              </span>
              <span className="tool-fill" />
              <IconButton size="small" label="Fold the to-do list" onClick={() => fold(true)}>
                <Minus size={14} />
              </IconButton>
            </div>
            <ul className="todo-list">
              {items.map((it, i) => (
                <li key={i} className={`todo-item ${it.status}`}>
                  {it.status === "done" ? <CircleCheck size={14} /> : it.status === "in_progress" ? <CircleDot size={14} /> : <Circle size={14} />}
                  <span>{it.text}</span>
                </li>
              ))}
            </ul>
          </motion.div>
        )}
      </AnimatePresence>
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
      {text.trim() ? text : live ? "Waiting for output…" : "No output."}
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

/** A change's hunks, in a box that scrolls. */
export function DiffView({ rows, truncated }: { rows: DiffRow[][]; truncated?: boolean }) {
  return (
    <div className="diff">
      <div className="diff-scroll">
        {rows.map((h, i) => (
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
        {truncated && <div className="diff-cut">The rest of this change is too long to show here.</div>}
      </div>
    </div>
  );
}
