import { getCurrentWebview } from "@tauri-apps/api/webview";
import { AnimatePresence, motion } from "framer-motion";
import { ArrowDown, Paperclip, Plug } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { reveal, rowSpring, spring } from "../lib/motion";
import { useStore } from "../lib/store";
import { Composer } from "./Composer";
import { MessageView } from "./MessageView";
import { Button, Empty } from "./ui";

export function ChatView() {
  const currentId = useStore((s) => s.currentId);
  const current = useStore((s) => s.current);
  const streaming = useStore((s) => (s.currentId ? !!s.streaming[s.currentId] : false));
  const providers = useStore((s) => s.providers);
  const loading = currentId !== null && current?.id !== currentId;

  if (!providers.length) {
    return (
      <div className="chat-view center">
        <Empty
          icon={<Plug size={22} />}
          title="Connect a model"
          action={
            <Button variant="primary" onClick={() => useStore.getState().openSettings("models", "new")}>
              Add a provider
            </Button>
          }
        >
          Relay talks to local servers like Ollama and LM Studio, or to any OpenAI-compatible API.
        </Empty>
      </div>
    );
  }

  if (!currentId) {
    return (
      <div className="chat-view center">
        <motion.div className="hero" initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} transition={reveal}>
          <h1>What’s on your mind?</h1>
          <div className="hero-composer">
            <Composer chatId={null} streaming={false} />
          </div>
        </motion.div>
        <DropZone />
      </div>
    );
  }

  return (
    <div className="chat-view">
      {loading ? <div className="messages" /> : <Messages key={current!.id} streaming={streaming} />}
      <div className="dock">
        <Composer chatId={currentId} streaming={streaming} />
      </div>
      <DropZone />
    </div>
  );
}

/** Files dragged over the chat: a target, and they go to the composer when dropped. */
function DropZone() {
  const [over, setOver] = useState(false);
  useEffect(() => {
    const off = getCurrentWebview().onDragDropEvent((e) => {
      const p = e.payload;
      if (p.type === "enter") setOver(p.paths.length > 0);
      else if (p.type === "leave") setOver(false);
      else if (p.type === "drop") {
        setOver(false);
        if (p.paths.length) void useStore.getState().attach({ paths: p.paths });
      }
    });
    return () => void off.then((un) => un());
  }, []);
  return (
    <AnimatePresence>
      {over && (
        <motion.div
          className="drop-zone"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0, transition: { duration: 0.12 } }}
          transition={{ duration: 0.12 }}
        >
          <motion.div className="drop-card" initial={{ scale: 0.96 }} animate={{ scale: 1 }} transition={spring}>
            <Paperclip size={20} />
            <span>Drop to attach</span>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}

/** How close to the bottom still counts as "following along". */
const STICK = 80;

function Messages({ streaming }: { streaming: boolean }) {
  const messages = useStore((s) => s.current?.messages ?? []);
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const [away, setAway] = useState(false);

  const toBottom = useCallback((behavior: ScrollBehavior = "auto") => {
    const el = scroller.current;
    if (el) el.scrollTo({ top: el.scrollHeight, behavior });
  }, []);

  // Open at the end of the conversation.
  useLayoutEffect(() => toBottom(), [toBottom]);

  // Follow new text while the reader is at the bottom.
  useLayoutEffect(() => {
    if (stick.current) toBottom();
  }, [messages, toBottom]);

  // Sending always brings you down to your message.
  const count = messages.length;
  useEffect(() => {
    stick.current = true;
    setAway(false);
    toBottom("smooth");
  }, [count, toBottom]);

  // Content that grows on its own (code highlighting, math, the reasoning folding).
  useEffect(() => {
    const el = scroller.current?.firstElementChild;
    if (!el) return;
    const ro = new ResizeObserver(() => stick.current && toBottom());
    ro.observe(el);
    return () => ro.disconnect();
  }, [toBottom]);

  return (
    <div
      ref={scroller}
      className="messages"
      onScroll={(e) => {
        const el = e.currentTarget;
        const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < STICK;
        stick.current = atBottom;
        if (away === atBottom) setAway(!atBottom);
      }}
      onWheel={(e) => {
        // Scrolling up mid-reply lets go right away, before the next batch pulls you down.
        if (e.deltaY < 0) stick.current = false;
      }}
    >
      <div className="thread">
        {messages.map((m, i) => (
          <motion.div
            key={m.id}
            initial={i >= messages.length - 2 && m.createdAt > Date.now() - 4000 ? { opacity: 0, y: 10 } : false}
            animate={{ opacity: 1, y: 0 }}
            transition={rowSpring}
          >
            <MessageView message={m} busy={streaming} />
          </motion.div>
        ))}
      </div>
      <AnimatePresence>
        {away && (
          <motion.button
            className={`to-bottom ${streaming ? "live" : ""}`}
            aria-label="Scroll to the end"
            initial={{ opacity: 0, y: 8, scale: 0.9 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 8, scale: 0.9 }}
            transition={spring}
            whileTap={{ scale: 0.92 }}
            onClick={() => {
              stick.current = true;
              setAway(false);
              toBottom("smooth");
            }}
          >
            <ArrowDown size={16} />
          </motion.button>
        )}
      </AnimatePresence>
    </div>
  );
}
