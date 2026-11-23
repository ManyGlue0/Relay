import { motion } from "framer-motion";
import {
  ChevronRight,
  CircleAlert,
  CircleCheck,
  CircleSlash,
  FilePen,
  FilePlus2,
  FileText,
  FolderTree,
  Loader,
  OctagonX,
  Search,
  ShieldQuestion,
  Trash2,
} from "lucide-react";
import { useState } from "react";
import type { DiffRow, Part, ToolDecision, ToolDetail } from "../lib/api";
import { spring } from "../lib/motion";
import { useStore } from "../lib/store";
import { seconds } from "../lib/time";
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
    default:
      return { icon: <FileText size={13} />, name: part.tool, verb: "Agent", ask: "Let the model do this?" };
  }
}

/** Steps whose approval can switch the session to Accept edits. */
const EDITS = new Set(["write_file", "edit_file", "delete_path"]);

export function AgentToolCard({ part, messageId }: { part: ToolPart; messageId: string }) {
  const [open, setOpen] = useState(false);
  const detail: ToolDetail = part.detail ?? {};
  const f = face(part, detail);
  const st = STATUS[part.status] ?? STATUS.running;
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
            {EDITS.has(part.tool) && !detail.note && (
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
      {open && !asking && (
        <div className="tool-body">
          {part.result != null && <pre className="tool-result">{part.result || "(empty)"}</pre>}
          {part.result == null && <pre className="tool-args">{part.arguments}</pre>}
        </div>
      )}
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
