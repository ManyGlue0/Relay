import { AnimatePresence, motion } from "framer-motion";
import { Check, CircleAlert, MessageCircleQuestion } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { api, errorText, type Answer, type Part, type Question } from "../lib/api";
import { spring } from "../lib/motion";
import { useStore } from "../lib/store";
import { Button, Pill } from "./ui";

type ToolPart = Extract<Part, { type: "tool" }>;

/** One question's answer while you choose: the options picked, and "Other"
 *  (an option like the rest, with its own text). */
interface Draft {
  picked: string[];
  otherOn: boolean;
  other: string;
}

const empty = (): Draft => ({ picked: [], otherOn: false, other: "" });
const answered = (d: Draft) => d.picked.length > 0 || (d.otherOn && d.other.trim() !== "");
/** What goes back: "Other"'s text only when Other is picked. */
const toAnswer = (d: Draft): Answer => ({ picked: d.picked, other: d.otherOn ? d.other.trim() : "" });

/** Answers in progress survive switching chats (by tool call). */
const drafts = new Map<string, Draft[]>();

/** The question the conversation is waiting on, if any. */
function usePending(): { messageId: string; part: ToolPart } | null {
  return useStore(
    useShallow((s) => {
      const c = s.current && s.current.id === s.currentId ? s.current : null;
      if (!c) return null;
      for (let m = c.messages.length - 1; m >= 0; m--) {
        const p = (c.messages[m].parts ?? []).find((x): x is ToolPart => x.type === "tool" && x.tool === "ask_user" && x.status === "question");
        if (p) return { messageId: c.messages[m].id, part: p };
      }
      return null;
    }),
  );
}

/** The model's questions, docked above the composer: a tab per question,
 *  then Submit with a recap. */
export function QuestionDock() {
  const pending = usePending();
  return <AnimatePresence initial={false}>{pending && <Questions key={pending.part.id} messageId={pending.messageId} part={pending.part} />}</AnimatePresence>;
}

function Questions({ messageId, part }: { messageId: string; part: ToolPart }) {
  const qs: Question[] = part.detail?.questions ?? [];
  const [list, setList] = useState<Draft[]>(() => drafts.get(part.id) ?? qs.map(empty));
  const [tab, setTab] = useState(0);
  const [sending, setSending] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  const otherInput = useRef<HTMLInputElement>(null);
  const submitTab = qs.length;

  useEffect(() => {
    drafts.set(part.id, list);
  }, [part.id, list]);
  // It asks for attention: keys go to it.
  useEffect(() => {
    box.current?.focus({ preventScroll: true });
  }, []);

  const change = (i: number, f: (d: Draft) => Draft) => setList((all) => all.map((d, j) => (j === i ? f(d) : d)));
  const next = () => setTab((t) => Math.min(t + 1, submitTab));

  const pick = (i: number, label: string) => {
    const q = qs[i];
    if (q.multiple) {
      change(i, (d) => ({ ...d, picked: d.picked.includes(label) ? d.picked.filter((x) => x !== label) : [...d.picked, label] }));
      return;
    }
    change(i, (d) => ({ ...d, picked: [label], otherOn: false }));
    // One choice made: on to the next question.
    window.setTimeout(next, 220);
  };
  const pickOther = (i: number) => {
    const q = qs[i];
    change(i, (d) => (q.multiple ? { ...d, otherOn: !d.otherOn } : { ...d, picked: [], otherOn: true }));
    window.setTimeout(() => otherInput.current?.focus(), 0);
  };

  const send = async (value: Answer[] | null) => {
    setSending(true);
    try {
      await api.answerTool(messageId, part.id, value);
      drafts.delete(part.id);
    } catch (e) {
      useStore.getState().toast(errorText(e));
      setSending(false);
    }
  };

  const onKey = (e: React.KeyboardEvent) => {
    if ((e.target as HTMLElement).tagName === "INPUT") return;
    if (e.key === "ArrowRight") setTab((t) => Math.min(t + 1, submitTab));
    else if (e.key === "ArrowLeft") setTab((t) => Math.max(t - 1, 0));
    else if (tab < submitTab && /^[1-9]$/.test(e.key)) {
      const n = Number(e.key) - 1;
      const opts = qs[tab].options;
      if (n < opts.length) pick(tab, opts[n].label);
      else if (n === opts.length) pickOther(tab);
    } else return;
    e.preventDefault();
  };

  const anyAnswered = list.some(answered);
  return (
    <motion.div
      ref={box}
      tabIndex={-1}
      className="qdock"
      onKeyDown={onKey}
      initial={{ opacity: 0, y: 12 }}
      animate={{ opacity: 1, y: 0 }}
      exit={{ opacity: 0, y: 8, transition: { duration: 0.14 } }}
      transition={spring}
    >
      <div className="qdock-head">
        <span className="qdock-icon">
          <MessageCircleQuestion size={16} />
        </span>
        <span className="qdock-title">{qs.length === 1 ? "A question for you" : `${qs.length} questions for you`}</span>
      </div>
      <div className="qdock-tabs" role="tablist">
        {qs.map((q, i) => (
          <button key={i} role="tab" aria-selected={tab === i} className={`qdock-tab ${tab === i ? "on" : ""}`} onClick={() => setTab(i)}>
            {tab === i && <Pill layoutId={`qtab-${part.id}`} className="segmented-pill" />}
            <span className="qdock-tab-label">
              {answered(list[i]) && <Check size={12} strokeWidth={3} className="qdock-done" />}
              {q.header || `Question ${i + 1}`}
            </span>
          </button>
        ))}
        <button role="tab" aria-selected={tab === submitTab} className={`qdock-tab ${tab === submitTab ? "on" : ""}`} onClick={() => setTab(submitTab)}>
          {tab === submitTab && <Pill layoutId={`qtab-${part.id}`} className="segmented-pill" />}
          <span className="qdock-tab-label">Submit</span>
        </button>
      </div>

      <div className="qdock-body">
        {tab < submitTab ? (
          <QuestionTab q={qs[tab]} draft={list[tab]} onPick={(label) => pick(tab, label)} onOther={() => pickOther(tab)} onOtherText={(t) =>
              change(tab, (d) => {
                // Writing in Other picks it (in a one-choice question, instead of the rest).
                const on = t.trim() !== "" || d.otherOn;
                return { ...d, other: t, otherOn: on, picked: on && !qs[tab].multiple ? [] : d.picked };
              })
            } onEnter={next} otherInput={otherInput} />
        ) : (
          <div className="qdock-recap">
            {qs.map((q, i) => {
              const d = list[i];
              const a = toAnswer(d);
              const text = [...a.picked, ...(a.other ? [`“${a.other}”`] : [])].join(", ");
              return (
                <button key={i} className="qdock-recap-row" onClick={() => setTab(i)} title="Change this answer">
                  <span className="qdock-recap-q">{q.question}</span>
                  {answered(d) ? (
                    <span className="qdock-recap-a">{text}</span>
                  ) : (
                    <span className="qdock-recap-a missing">
                      <CircleAlert size={12} />
                      Not answered
                    </span>
                  )}
                </button>
              );
            })}
          </div>
        )}
      </div>

      <div className="qdock-bar">
        <Button variant="ghost" disabled={sending} onClick={() => void send(null)} title="Let the model decide by itself">
          Skip
        </Button>
        <span className="tool-fill" />
        {tab < submitTab ? (
          <Button variant="default" onClick={next}>
            Next
          </Button>
        ) : (
          <Button variant="primary" disabled={!anyAnswered || sending} onClick={() => void send(list.map(toAnswer))}>
            Submit answers
          </Button>
        )}
      </div>
    </motion.div>
  );
}

function QuestionTab({
  q,
  draft,
  onPick,
  onOther,
  onOtherText,
  onEnter,
  otherInput,
}: {
  q: Question;
  draft: Draft;
  onPick: (label: string) => void;
  onOther: () => void;
  onOtherText: (t: string) => void;
  onEnter: () => void;
  otherInput: React.RefObject<HTMLInputElement | null>;
}) {
  return (
    <div className="qdock-question">
      <div className="qdock-text">{q.question}</div>
      <div className="qdock-hint">{q.multiple ? "Pick any that apply" : "Pick one"}</div>
      <div className="question-options">
        {q.options.map((o, i) => {
          const on = draft.picked.includes(o.label);
          return (
            <button key={o.label} className={`question-option ${on ? "on" : ""}`} onClick={() => onPick(o.label)} aria-pressed={on}>
              <Mark multiple={q.multiple} on={on} />
              <span className="question-option-text">
                <span className="question-label">{o.label}</span>
                {o.description && <span className="question-desc">{o.description}</span>}
              </span>
              <span className="qdock-key">{i + 1}</span>
            </button>
          );
        })}
        <div className={`question-option qdock-other ${draft.otherOn ? "on" : ""}`} onClick={onOther} role="button" aria-pressed={draft.otherOn}>
          <Mark multiple={q.multiple} on={draft.otherOn} />
          <input
            ref={otherInput}
            className="qdock-other-input"
            placeholder="Other: write your own answer"
            value={draft.other}
            onClick={(e) => {
              e.stopPropagation();
              if (!draft.otherOn) onOther();
            }}
            onChange={(e) => onOtherText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && draft.other.trim()) onEnter();
            }}
          />
          <span className="qdock-key">{q.options.length + 1}</span>
        </div>
      </div>
    </div>
  );
}

/** A radio's dot or a checkbox's tick, drawn on a 16 px grid so both sit dead center. */
function Mark({ multiple, on }: { multiple: boolean; on: boolean }) {
  return (
    <span className={`question-mark ${multiple ? "qm-box" : "qm-dot"}`}>
      {on && multiple && (
        <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden>
          <path d="M4.5 8.25 7 10.75 11.5 5.75" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      )}
    </span>
  );
}
