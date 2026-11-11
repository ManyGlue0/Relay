import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Bot, Brain, Cloud, Cpu, Globe, HardDrive, ImagePlus, Server, Sparkles, Wand2, Zap, type LucideIcon } from "lucide-react";
import { useRef, useState } from "react";
import anthropic from "../assets/providers/anthropic.svg?raw";
import grok from "../assets/providers/grok.svg?raw";
import lmstudio from "../assets/providers/lmstudio.svg?raw";
import ollama from "../assets/providers/ollama.svg?raw";
import openai from "../assets/providers/openai.svg?raw";
import openrouter from "../assets/providers/openrouter.svg?raw";
import vllm from "../assets/providers/vllm.svg?raw";
import { api, errorText } from "../lib/api";
import { useStore } from "../lib/store";
import { Popover, anchorFrom, type Anchor } from "./ui";

/** An SVG ready to inline: without its <title> (it would show as a tooltip). */
const clean = (svg: string) => svg.replace(/<title>[\s\S]*?<\/title>/g, "");

/** Official marks (from LobeHub's MIT icon set, one color: they follow the theme). */
export const BRANDS: { id: string; name: string; svg: string }[] = [
  { id: "openai", name: "OpenAI", svg: clean(openai) },
  { id: "anthropic", name: "Anthropic", svg: clean(anthropic) },
  { id: "grok", name: "SpaceXAI", svg: clean(grok) },
  { id: "openrouter", name: "OpenRouter", svg: clean(openrouter) },
  { id: "ollama", name: "Ollama", svg: clean(ollama) },
  { id: "lmstudio", name: "LM Studio", svg: clean(lmstudio) },
  { id: "vllm", name: "vLLM", svg: clean(vllm) },
];

const PLAIN: Record<string, LucideIcon> = {
  server: Server,
  cpu: Cpu,
  "hard-drive": HardDrive,
  cloud: Cloud,
  globe: Globe,
  bot: Bot,
  brain: Brain,
  sparkles: Sparkles,
  zap: Zap,
  wand: Wand2,
};

/** The icon a provider gets until someone picks one: its service's mark when
 *  the address (or name) says which service it is, else a server. */
export function autoIcon(baseUrl: string, name = ""): string {
  const u = baseUrl.toLowerCase();
  const n = name.toLowerCase();
  if (u.includes("api.openai.com")) return "brand:openai";
  if (u.includes("api.anthropic.com")) return "brand:anthropic";
  if (u.includes("api.x.ai")) return "brand:grok";
  if (u.includes("openrouter.ai")) return "brand:openrouter";
  if (u.includes(":11434") || n.includes("ollama")) return "brand:ollama";
  if (u.includes(":1234") || n.includes("lm studio") || n.includes("lmstudio")) return "brand:lmstudio";
  if (n.includes("vllm")) return "brand:vllm";
  if (n.includes("llama.cpp") || n.includes("llama-server")) return "lucide:cpu";
  return "lucide:server";
}

export function iconOf(p: { icon?: string | null; baseUrl: string; name: string }): string {
  return p.icon || autoIcon(p.baseUrl, p.name);
}

/** Draws an icon value: "brand:…", "lucide:…" or an image. */
export function IconGlyph({ icon, size = 15 }: { icon: string; size?: number }) {
  if (icon.startsWith("data:image/")) return <img className="provider-img" src={icon} alt="" width={size + 5} height={size + 5} draggable={false} />;
  if (icon.startsWith("brand:")) {
    const b = BRANDS.find((x) => x.id === icon.slice(6));
    if (b) return <span className="brand-glyph" style={{ width: size, height: size }} dangerouslySetInnerHTML={{ __html: b.svg }} aria-hidden />;
  }
  const Plain = PLAIN[icon.slice(7)] ?? Server;
  return <Plain size={size} />;
}

/** A provider's icon on its tile: logos on a neutral tile, plain icons on the accent one. */
export function ProviderIcon({ icon, size = 15, className = "" }: { icon: string; size?: number; className?: string }) {
  const kind = icon.startsWith("data:image/") ? "image" : icon.startsWith("brand:") ? "brand" : "plain";
  return (
    <span className={`well well-${kind} ${className}`}>
      <IconGlyph icon={icon} size={size} />
    </span>
  );
}

/** The icon button in the provider editor: logos, plain icons, your own image, or automatic. */
export function IconPicker({ value, auto, onChange }: { value: string | null; auto: string; onChange: (icon: string | null) => void }) {
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const close = () => setAnchor(null);
  const current = value || auto;
  const pick = (icon: string | null) => {
    close();
    onChange(icon);
  };
  const fromFile = async () => {
    close();
    const path = await openDialog({ multiple: false, filters: [{ name: "Images", extensions: ["png", "jpg", "jpeg", "webp", "gif", "svg", "ico"] }] });
    if (typeof path !== "string") return;
    try {
      onChange(await api.iconFromFile(path));
    } catch (e) {
      useStore.getState().toast(errorText(e));
    }
  };
  const cell = (icon: string, label: string) => (
    <button key={icon} className={`icon-cell ${current === icon && value ? "on" : ""}`} title={label} aria-label={label} onClick={() => pick(icon)}>
      <ProviderIcon icon={icon} size={16} />
    </button>
  );
  return (
    <>
      <button ref={btn} className={`icon-pick ${anchor ? "open" : ""}`} aria-label="Change the icon" title="Change the icon" onClick={() => (anchor ? close() : setAnchor(anchorFrom(btn.current!, { above: false })))}>
        <ProviderIcon icon={current} size={17} />
      </button>
      <Popover anchor={anchor} onClose={close} width={264} trigger={btn}>
        <div className="menu icon-menu">
          <div className="menu-section">Logos</div>
          <div className="icon-grid">{BRANDS.map((b) => cell(`brand:${b.id}`, b.name))}</div>
          <div className="menu-section">Icons</div>
          <div className="icon-grid">{Object.keys(PLAIN).map((k) => cell(`lucide:${k}`, k.replace("-", " ")))}</div>
          <div className="menu-sep" />
          <button className="menu-item" onClick={() => void fromFile()}>
            <span className="menu-icon">
              <ImagePlus size={15} />
            </span>
            <span className="menu-label">Choose an image…</span>
          </button>
          <button className="menu-item" onClick={() => pick(null)} disabled={!value}>
            <span className="menu-icon">
              <IconGlyph icon={auto} size={15} />
            </span>
            <span className="menu-label">Automatic</span>
            <span className="menu-hint">From the address</span>
          </button>
        </div>
      </Popover>
    </>
  );
}
