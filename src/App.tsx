import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { flushSync } from "react-dom";
import { api, on, type ChatSummary, type McpStatus, type Message, type StreamDelta } from "./lib/api";
import { handleEscape, useDark, useStore } from "./lib/store";
import { ChatView } from "./components/ChatView";
import { FilesPanel } from "./components/FilesPanel";
import { SettingsPage } from "./components/settings/SettingsPage";
import { Sidebar } from "./components/Sidebar";
import { RAIL, TitleBar } from "./components/TitleBar";
import { Toasts } from "./components/ui";

const MIN_SIDEBAR = 220;
const MAX_SIDEBAR = 420;
const WIDTHS = { narrow: 640, normal: 760, wide: 960 } as const;

export default function App() {
  const loaded = useStore((s) => s.loaded);
  const sidebarOpen = useStore((s) => s.settings?.sidebarOpen ?? true);
  const savedWidth = useStore((s) => s.settings?.sidebarWidth ?? 272);
  const textSize = useStore((s) => s.settings?.textSize ?? 14);
  const chatWidth = useStore((s) => s.settings?.chatWidth ?? "normal");
  const settingsOpen = useStore((s) => s.settingsOpen);
  const [width, setWidth] = useState(savedWidth);
  const [resizing, setResizing] = useState(false);
  const dark = useDark();
  const search = useRef<HTMLInputElement>(null);

  useEffect(() => setWidth(savedWidth), [savedWidth]);

  // ---- boot + backend events ----
  useEffect(() => {
    void useStore.getState().init();
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const onMedia = () => useStore.setState({ systemDark: media.matches });
    media.addEventListener("change", onMedia);

    const win = getCurrentWindow();
    const syncMax = () => win.isMaximized().then((m) => useStore.setState({ maximized: m }));
    void syncMax();
    const unResize = win.onResized(() => void syncMax());

    const onFocus = () => {
      useStore.setState({ windowActive: true });
      const id = useStore.getState().currentId;
      if (id) void api.holsterSeen(id);
    };
    const onBlur = () => useStore.setState({ windowActive: false });
    window.addEventListener("focus", onFocus);
    window.addEventListener("blur", onBlur);

    const offs = [
      on<StreamDelta>("stream-delta", (d) => useStore.getState().applyDelta(d)),
      on<Message>("stream-end", (m) => useStore.getState().applyEnd(m)),
      on<ChatSummary>("chat-updated", (c) => useStore.getState().applyChat(c)),
      on<McpStatus>("mcp-status", (st) => useStore.getState().applyMcpStatus(st)),
      on("mcp-servers-changed", () => void useStore.getState().refreshMcp()),
      on("skills-changed", () => void useStore.getState().refreshSkills()),
      on("agent-blocked", () => useStore.setState((s) => ({ blockedSeq: s.blockedSeq + 1 }))),
      on<{ chatId: string; waiting: boolean }>("chat-waiting", (w) => useStore.setState((s) => ({ waiting: { ...s.waiting, [w.chatId]: w.waiting } }))),
      on<Message>("stream-end", (m) => useStore.setState((s) => (s.waiting[m.chatId] ? { waiting: { ...s.waiting, [m.chatId]: false } } : {}))),
      on<string>("open-chat", (id) => void useStore.getState().openChat(id)),
      on<string>("navigate", (to) => {
        if (to === "settings") useStore.getState().openSettings();
        if (to === "new") useStore.getState().newChat();
      }),
    ];

    return () => {
      offs.forEach((off) => off());
      media.removeEventListener("change", onMedia);
      window.removeEventListener("focus", onFocus);
      window.removeEventListener("blur", onBlur);
      void unResize.then((u) => u());
    };
  }, []);

  // ---- theme (crossfades) ----
  useLayoutEffect(() => {
    const root = document.documentElement;
    const next = dark ? "dark" : "light";
    if (root.dataset.theme === next) return;
    const doc = document as Document & { startViewTransition?: (cb: () => void) => unknown };
    if (root.dataset.theme && doc.startViewTransition) {
      doc.startViewTransition(() => flushSync(() => (root.dataset.theme = next)));
    } else {
      root.dataset.theme = next;
    }
  }, [dark]);

  // ---- keyboard ----
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.isComposing) return;
      const st = useStore.getState();
      const target = e.target as HTMLElement;
      const inField = target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable;
      const consume = () => {
        e.preventDefault();
        e.stopPropagation();
      };
      if (e.key === "Escape") {
        if (handleEscape()) return consume();
        if (st.currentId && st.streaming[st.currentId]) {
          st.stop();
          return consume();
        }
        return;
      }
      const ctrl = e.ctrlKey && !e.altKey && !e.metaKey;
      const k = e.key.toLowerCase();
      if (ctrl && !e.shiftKey && k === "n") {
        consume();
        st.newChat();
      } else if (ctrl && !e.shiftKey && (k === "k" || (k === "f" && !inField))) {
        consume();
        if (st.settingsOpen) st.closeSettings();
        if (!st.settings?.sidebarOpen) void st.updateSettings({ sidebarOpen: true });
        setTimeout(() => search.current?.select(), 60);
      } else if (ctrl && !e.shiftKey && k === "b") {
        consume();
        void st.updateSettings({ sidebarOpen: !st.settings?.sidebarOpen });
      } else if (ctrl && e.key === ",") {
        consume();
        if (st.settingsOpen) st.closeSettings();
        else st.openSettings();
      } else if (!inField && !st.settingsOpen && !e.ctrlKey && !e.altKey && !e.metaKey && e.key.length === 1 && !document.querySelector(".popover, .modal")) {
        // Typing anywhere goes to the message box.
        document.querySelector<HTMLTextAreaElement>(".composer textarea")?.focus();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  if (!loaded) return <div className="boot" />;

  const sideWidth = sidebarOpen ? width : RAIL;
  const vars = { "--chat-size": `${textSize}px`, "--thread-width": `${WIDTHS[chatWidth] ?? 760}px` } as React.CSSProperties;

  return (
    <div className="app" style={vars}>
      <TitleBar sidebarWidth={width} />
      <div className="body">
        {/* The content keeps its full width while the panel narrows: nothing reflows. */}
        <div className={`sidebar-wrap ${resizing ? "resizing" : ""}`} style={{ width: sideWidth }}>
          <div className="sidebar-inner" style={{ width }}>
            <Sidebar ref={search} rail={!sidebarOpen} />
          </div>
          {sidebarOpen && <Resizer width={width} onChange={setWidth} onResizing={setResizing} />}
        </div>
        <main className="pane">
          {settingsOpen ? (
            <SettingsPage />
          ) : (
            <div className="pane-row">
              <ChatView />
              <FilesPanel />
            </div>
          )}
        </main>
      </div>
      <Toasts />
    </div>
  );
}

function Resizer({ width, onChange, onResizing }: { width: number; onChange: (w: number) => void; onResizing: (on: boolean) => void }) {
  return (
    <div
      className="resizer"
      onPointerDown={(e) => {
        e.preventDefault();
        const startX = e.clientX;
        const start = width;
        let last = width;
        onResizing(true);
        const move = (ev: PointerEvent) => {
          last = Math.round(Math.max(MIN_SIDEBAR, Math.min(MAX_SIDEBAR, start + ev.clientX - startX)));
          onChange(last);
        };
        const up = () => {
          window.removeEventListener("pointermove", move);
          window.removeEventListener("pointerup", up);
          document.body.classList.remove("resizing");
          onResizing(false);
          void useStore.getState().updateSettings({ sidebarWidth: last });
        };
        document.body.classList.add("resizing");
        window.addEventListener("pointermove", move);
        window.addEventListener("pointerup", up);
      }}
      onDoubleClick={() => {
        onChange(272);
        void useStore.getState().updateSettings({ sidebarWidth: 272 });
      }}
    />
  );
}
