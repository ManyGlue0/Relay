import { getCurrentWindow } from "@tauri-apps/api/window";
import { Copy as RestoreIcon, Minus, PanelLeft, Square, X } from "lucide-react";
import { useId } from "react";
import { api } from "../lib/api";
import { useStore } from "../lib/store";
import { SECTIONS } from "./Sidebar";
import { IconButton } from "./ui";

/** The collapsed sidebar's width: its icons sit on the same column as the open sidebar's. */
export const RAIL = 54;
/** The title bar's fixed left group (mark, name, sidebar button). */
const BRAND = 132;

export function TitleBar({ sidebarWidth }: { sidebarWidth: number }) {
  const maximized = useStore((s) => s.maximized);
  const open = useStore((s) => s.settings?.sidebarOpen ?? true);
  const settingsOpen = useStore((s) => s.settingsOpen);
  const section = useStore((s) => s.section);
  const chatTitle = useStore((s) => (s.currentId ? (s.chats.find((c) => c.id === s.currentId)?.title ?? s.current?.title ?? "") : ""));
  const title = settingsOpen ? `Settings · ${SECTIONS.find((x) => x.id === section)?.label ?? ""}` : chatTitle;
  // The title starts where the conversation does, and travels with it.
  const titleOffset = Math.max(4, (open ? sidebarWidth : RAIL) + 16 - BRAND);

  return (
    <header className="titlebar" data-tauri-drag-region>
      <div className="brand" style={{ width: BRAND }} data-tauri-drag-region>
        <BrandMark />
        <span className="wordmark" data-tauri-drag-region>
          Relay
        </span>
        <IconButton label={open ? "Collapse the sidebar (Ctrl+B)" : "Expand the sidebar (Ctrl+B)"} onClick={() => void useStore.getState().updateSettings({ sidebarOpen: !open })}>
          <PanelLeft size={16} />
        </IconButton>
      </div>
      <div className="titlebar-title" style={{ marginLeft: titleOffset }} data-tauri-drag-region>
        <span data-tauri-drag-region>{title}</span>
      </div>
      <div className="caption">
        <button className="caption-btn" aria-label="Minimize" onClick={() => void getCurrentWindow().minimize()}>
          <Minus size={15} strokeWidth={1.6} />
        </button>
        <button className="caption-btn" aria-label={maximized ? "Restore" : "Maximize"} onClick={() => void getCurrentWindow().toggleMaximize()}>
          {maximized ? <RestoreIcon size={12} strokeWidth={1.7} style={{ transform: "scaleX(-1)" }} /> : <Square size={12} strokeWidth={1.7} />}
        </button>
        <button className="caption-btn close" aria-label="Close" onClick={() => void api.closeWindow()}>
          <X size={16} strokeWidth={1.6} />
        </button>
      </div>
    </header>
  );
}

/** The app mark, as in the icon (two bubbles, no background). */
export function BrandMark({ size = 20 }: { size?: number }) {
  const mask = `brand-gap-${useId().replace(/:/g, "")}`;
  return (
    <svg viewBox="112 132 800 800" width={size} height={size} aria-hidden>
      <defs>
        <mask id={mask} maskUnits="userSpaceOnUse" x="0" y="0" width="1024" height="1024">
          <rect width="1024" height="1024" fill="#fff" />
          <path
            d="M232 380h318c58 0 104 46 104 104v206c0 58-46 104-104 104H366l-122 100v-100h-12c-58 0-104-46-104-104V484c0-58 46-104 104-104z"
            fill="#000"
            stroke="#000"
            strokeWidth="64"
            strokeLinejoin="round"
          />
        </mask>
      </defs>
      <path
        mask={`url(#${mask})`}
        d="M474 150h318c58 0 104 46 104 104v206c0 58-46 104-104 104h-28v100L642 564H474c-58 0-104-46-104-104V254c0-58 46-104 104-104z"
        fill="#B2A2F5"
      />
      <path d="M232 380h318c58 0 104 46 104 104v206c0 58-46 104-104 104H366l-122 100v-100h-12c-58 0-104-46-104-104V484c0-58 46-104 104-104z" fill="#89BCF2" />
      <circle cx="282" cy="587" r="38" fill="#1E2F47" />
      <circle cx="391" cy="587" r="38" fill="#1E2F47" />
      <circle cx="500" cy="587" r="38" fill="#1E2F47" />
    </svg>
  );
}
