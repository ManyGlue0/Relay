import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type Theme = "system" | "light" | "dark";
export type Effort = "off" | "low" | "medium" | "high" | "xhigh" | "max";
export type ToolPolicy = "ask" | "allow" | "off";

export interface ModelRef {
  providerId: string;
  model: string;
}

export interface Profile {
  name: string;
  nickname: string;
  pronouns: string;
  location: string;
  occupation: string;
  about: string;
  style: string;
  language: string;
}

export interface Settings {
  theme: Theme;
  textSize: number;
  chatWidth: "narrow" | "normal" | "wide";
  autoTitles: boolean;
  sendKey: "enter" | "ctrl-enter";
  expandThinking: boolean;
  showStats: boolean;
  defaultModel: ModelRef | null;
  defaultEffort: Effort;
  defaultPreset: string | null;
  /** The model that names chats; null: the chat's own. */
  titleModel: ModelRef | null;
  profile: Profile;
  shareProfile: boolean;
  shareDate: boolean;
  notifyOnReply: boolean;
  holsterGlow: boolean;
  toolRounds: number;
  /** Send each earlier reply's reasoning back as `reasoning_content`. */
  sendReasoning: boolean;
  launchAtStartup: boolean;
  closeAction: "tray" | "quit";
  sidebarWidth: number;
  sidebarOpen: boolean;
  /** Skills (folder names) models don't get. */
  disabledSkills: string[];
  /** Skills whose scripts run without asking. */
  skillScripts: string[];
}

/** A skill: a folder with SKILL.md (and maybe more files). */
export interface Skill {
  id: string;
  name: string;
  description: string;
  body: string;
  enabled: boolean;
  scriptsAllowed: boolean;
  files: string[];
  scripts: string[];
  path: string;
  error: string | null;
}

export interface SkillsInfo {
  prompt: string;
  folder: string;
  workspace: string;
}

/** The built-in skill tools' server id (see skills.rs). */
export const SKILLS_SERVER = "relay.skills";

export interface Provider {
  id: string;
  name: string;
  baseUrl: string;
  models: string[];
  modelsAt: number;
  customModels: string[];
  /** Show every model the server lists; off: only `customModels` (the ones ticked or typed). */
  showAll: boolean;
  /** "brand:openai", "lucide:cpu" or an image data URL; null: chosen from the address. */
  icon: string | null;
  contexts: Record<string, number>;
  hasKey: boolean;
  sort: number;
}

/** Whether a provider answered just now, and what it lists (null: it doesn't list models). */
export interface ProviderStatus {
  id: string;
  online: boolean;
  models: string[] | null;
}

export interface Preset {
  id: string;
  name: string;
  systemPrompt: string;
  temperature: number | null;
  topP: number | null;
  maxTokens: number | null;
  effort: Effort | null;
  sort: number;
}

export interface ToolInfo {
  name: string;
  title?: string | null;
  description: string;
  inputSchema: unknown;
}

export interface McpStatus {
  id: string;
  state: "off" | "connecting" | "ready" | "error";
  error: string | null;
  tools: ToolInfo[];
  serverName: string | null;
  serverVersion: string | null;
}

export interface McpServer {
  id: string;
  name: string;
  kind: "stdio" | "http";
  command: string;
  args: string[];
  cwd: string;
  url: string;
  enabled: boolean;
  toolPolicy: Record<string, ToolPolicy>;
  defaultPolicy: ToolPolicy;
  envKeys: string[];
  hasToken: boolean;
  sort: number;
}

export interface McpView extends McpServer {
  status: McpStatus;
}

export interface ChatSummary {
  id: string;
  title: string;
  providerId: string | null;
  model: string | null;
  createdAt: number;
  updatedAt: number;
  pinned: boolean;
  presetId: string | null;
  effort: Effort | null;
  tools: boolean;
  /** In the sidebar's Archived group. */
  archived: boolean;
}

export type AttachmentKind = "image" | "audio" | "video" | "pdf" | "document" | "text";

/** A file sent with a message (attachments.rs). */
export interface Attachment {
  id: string;
  name: string;
  kind: AttachmentKind;
  mime: string;
  size: number;
  /** Its name in the attachment's folder. */
  file: string;
  width?: number;
  height?: number;
  /** Seconds (audio, video). */
  duration?: number;
  /** Video: when each still was taken (x/frame-N.jpg). */
  frames?: number[];
  /** x/sound.wav exists. */
  sound?: boolean;
  /** PDFs and documents: characters of text read from it. */
  chars?: number;
  /** Recorded in Relay. */
  voice?: boolean;
}

export type Ability = "vision" | "audio" | "video" | "pdf";
export type Caps = Record<Ability, boolean>;

/** What a model takes: `caps` is what Relay goes by; `user`, what the user said over the detected `detected`. */
export interface ModelCaps {
  caps: Caps;
  detected: Caps;
  user: Partial<Record<Ability, boolean>>;
}

/** A URL the window can load for an attachment's file (or one of Relay's, like "x/frame-0.jpg"). */
export function fileUrl(a: Attachment, file: string = a.file): string {
  // convertFileSrc encodes the whole path (slashes too); serve() decodes it once.
  return convertFileSrc(`${a.id}/${file}`, "relay");
}

export interface MessageStats {
  ttftMs?: number | null;
  thinkMs?: number | null;
  totalMs?: number;
  promptTokens?: number | null;
  completionTokens?: number;
  estimated?: boolean;
  tokensPerSec?: number | null;
  finishReason?: string | null;
  toolCalls?: number;
}

export type MessageStatus = "done" | "streaming" | "stopped" | "error";

export type ToolStatus = "approval" | "running" | "done" | "error" | "denied";

export type Part =
  | { type: "reasoning"; text: string; ms?: number }
  | { type: "text"; text: string }
  | { type: "tool"; id: string; serverId: string; server: string; tool: string; arguments: string; status: ToolStatus; result?: string; ms?: number };

export interface Message {
  id: string;
  chatId: string;
  parentId: string | null;
  role: "user" | "assistant" | "system";
  content: string;
  reasoning: string;
  parts: Part[] | null;
  providerId: string | null;
  model: string | null;
  status: MessageStatus;
  error: string | null;
  stats: MessageStats | null;
  createdAt: number;
  /** Files sent with a user message. */
  attachments: Attachment[];
  /** Streaming replies: how many batches `parts` already holds. */
  seq: number;
  /** This message and its alternatives, oldest first. */
  siblings: string[];
  /** UI only: the model is reasoning right now. */
  thinking?: boolean;
}

export interface Chat extends ChatSummary {
  systemPrompt: string;
  leafId: string | null;
  messages: Message[];
}

export interface ChatOptions {
  presetId?: string | null;
  effort?: Effort | null;
  tools?: boolean;
}

export interface SearchHit {
  chatId: string;
  title: string;
  updatedAt: number;
  snippet: string;
  archived: boolean;
}

export interface HolsterStatus {
  installed: boolean;
  registered: boolean;
  error: string | null;
  warnings: string[];
  port: number;
  sideBySide: boolean;
}

export type Op =
  | { op: "append"; kind: "text" | "reasoning"; text: string }
  | { op: "tool"; part: Part }
  | { op: "thought"; index: number; ms: number };

export interface StreamDelta {
  chatId: string;
  messageId: string;
  seq: number;
  ops: Op[];
  thinking: boolean;
}

export interface ProviderInput {
  id?: string;
  name: string;
  baseUrl: string;
  customModels: string[];
  /** undefined keeps the stored key; "" removes it. */
  key?: string;
  models?: string[];
  contexts?: Record<string, number>;
  showAll?: boolean;
  /** undefined keeps the icon; "" goes back to automatic. */
  icon?: string;
}

export interface ModelUsage {
  providerId: string | null;
  model: string;
  replies: number;
  promptTokens: number;
  completionTokens: number;
  avgTokensPerSec: number | null;
  avgTtftMs: number | null;
  toolCalls: number;
  errors: number;
  lastUsed: number;
}

export interface Usage {
  chats: number;
  messages: number;
  replies: number;
  promptTokens: number;
  completionTokens: number;
  toolCalls: number;
  models: ModelUsage[];
  days: { day: string; replies: number; tokens: number }[];
}

export const api = {
  appInfo: () => invoke<{ version: string; dataDir: string }>("app_info"),
  updateCheck: () => invoke<{ version: string; notes: string | null } | null>("update_check"),
  updateInstall: () => invoke<void>("update_install"),
  iconFromFile: (path: string) => invoke<string>("icon_from_file", { path }),
  probeProviders: () => invoke<{ statuses: ProviderStatus[]; changed: boolean }>("providers_probe"),
  getSettings: () => invoke<Settings>("get_settings"),
  updateSettings: (patch: Partial<Settings>) => invoke<Settings>("update_settings", { patch }),
  systemPromptPreview: (presetId?: string | null) => invoke<string>("system_prompt_preview", { presetId }),

  providers: () => invoke<Provider[]>("providers_list"),
  saveProvider: (input: ProviderInput) => invoke<Provider>("provider_save", { input }),
  deleteProvider: (id: string) => invoke<void>("provider_delete", { id }),
  testProvider: (baseUrl: string, key?: string, id?: string) => invoke<string[]>("provider_test", { baseUrl, key, id }),
  refreshProvider: (id: string) => invoke<Provider>("provider_refresh", { id }),
  modelContext: (providerId: string, model: string) => invoke<number | null>("model_context", { providerId, model }),
  modelCaps: (providerId: string, model: string) => invoke<ModelCaps | null>("model_caps", { providerId, model }),
  /** `on` null: back to what was detected. */
  setModelCap: (providerId: string, model: string, ability: Ability, on: boolean | null) =>
    invoke<ModelCaps>("model_caps_set", { providerId, model, ability, on }),

  attachPath: (path: string) => invoke<Attachment>("attach_path", { path }),
  attachBytes: (name: string, bytes: Uint8Array, extra: { voice?: boolean; duration?: number } = {}) =>
    invoke<Attachment>("attach_bytes", bytes, {
      headers: { "x-name": encodeURIComponent(name), ...(extra.voice ? { "x-voice": "1" } : {}), ...(extra.duration ? { "x-duration": String(extra.duration) } : {}) },
    }),
  /** A still ("frame-N.jpg", taken at `at` seconds) or the sound ("sound.wav") Relay made of a file. */
  attachPut: (id: string, file: string, bytes: Uint8Array, at?: number) =>
    invoke<Attachment>("attach_put", bytes, { headers: { "x-id": id, "x-file": file, ...(at !== undefined ? { "x-at": String(at) } : {}) } }),
  attachInfo: (id: string, info: { duration?: number; width?: number; height?: number }) => invoke<Attachment>("attach_info", { id, ...info }),
  attachDiscard: (ids: string[]) => invoke<void>("attach_discard", { ids }),
  attachOpen: (id: string) => invoke<void>("attach_open", { id }),

  presets: () => invoke<Preset[]>("presets_list"),
  savePreset: (preset: Partial<Preset> & { name: string }) => invoke<Preset>("preset_save", { preset: { id: "", systemPrompt: "", sort: 0, temperature: null, topP: null, maxTokens: null, effort: null, ...preset } }),
  deletePreset: (id: string) => invoke<void>("preset_delete", { id }),

  mcp: () => invoke<McpView[]>("mcp_list"),
  saveMcp: (server: Partial<McpServer> & { name: string; kind: "stdio" | "http" }, env?: Record<string, string>, token?: string) =>
    invoke<McpServer>("mcp_save", { input: { id: "", command: "", args: [], cwd: "", url: "", enabled: true, toolPolicy: {}, defaultPolicy: "ask", envKeys: [], hasToken: false, sort: 0, ...server, env, token } }),
  mcpEnv: (id: string) => invoke<Record<string, string>>("mcp_env", { id }),
  deleteMcp: (id: string) => invoke<void>("mcp_delete", { id }),
  restartMcp: (id: string) => invoke<void>("mcp_restart", { id }),
  setMcpEnabled: (id: string, enabled: boolean) => invoke<void>("mcp_set_enabled", { id, enabled }),
  setToolPolicy: (id: string, tool: string | null, policy: ToolPolicy) => invoke<void>("mcp_set_policy", { id, tool, policy }),
  importMcp: (text: string) => invoke<number>("mcp_import", { text }),
  decideTool: (messageId: string, callId: string, decision: "once" | "always" | "deny") => invoke<void>("tool_decide", { messageId, callId, decision }),

  skills: () => invoke<Skill[]>("skills_list"),
  skillsInfo: () => invoke<SkillsInfo>("skills_info"),
  saveSkill: (id: string | null, name: string, description: string, body: string) => invoke<Skill>("skill_save", { id, name, description, body }),
  deleteSkill: (id: string) => invoke<void>("skill_delete", { id }),
  setSkill: (id: string, flags: { enabled?: boolean; scriptsAllowed?: boolean }) => invoke<void>("skill_set", { id, ...flags }),
  importSkill: (path: string) => invoke<{ skill: Skill; replaced: boolean }>("skill_import", { path }),
  openSkillsFolder: (which: string) => invoke<void>("skills_open_folder", { which }),

  chats: () => invoke<ChatSummary[]>("chats_list"),
  chat: (id: string) => invoke<Chat | null>("chat_get", { id }),
  renameChat: (id: string, title: string) => invoke<void>("chat_rename", { id, title }),
  pinChat: (id: string, pinned: boolean) => invoke<void>("chat_pin", { id, pinned }),
  archiveChat: (id: string, archived: boolean) => invoke<void>("chat_archive", { id, archived }),
  deleteChat: (id: string) => invoke<void>("chat_delete", { id }),
  deleteAllChats: () => invoke<void>("chats_delete_all"),
  setChatModel: (id: string, providerId: string, model: string) => invoke<void>("chat_set_model", { id, providerId, model }),
  setChatOptions: (id: string, options: ChatOptions) => invoke<ChatSummary | null>("chat_set_options", { id, options }),
  send: (args: { chatId: string | null; text: string; attachments: string[]; providerId: string; model: string; options?: ChatOptions }) =>
    invoke<{ chat: ChatSummary; user: Message | null; assistant: Message }>("chat_send", args),
  edit: (chatId: string, messageId: string, text: string, attachments: string[], providerId: string, model: string) =>
    invoke<Chat>("chat_edit", { chatId, messageId, text, attachments, providerId, model }),
  regenerate: (chatId: string, messageId: string, providerId: string, model: string) =>
    invoke<Chat>("chat_regenerate", { chatId, messageId, providerId, model }),
  branch: (chatId: string, messageId: string) => invoke<Chat>("chat_branch", { chatId, messageId }),
  stop: (chatId: string) => invoke<void>("chat_stop", { chatId }),
  streamingChats: () => invoke<string[]>("streaming_chats"),
  search: (query: string) => invoke<SearchHit[]>("chat_search", { query }),
  exportChat: (id: string, path: string) => invoke<void>("chat_export", { id, path }),
  /** `days` null: all time. */
  usage: (days: number | null) => invoke<Usage>("usage_stats", { days }),

  holsterStatus: () => invoke<HolsterStatus>("holster_status"),
  holsterSeen: (chatId: string) => invoke<void>("holster_seen", { chatId }),
  closeWindow: () => invoke<void>("window_close"),
  quit: () => invoke<void>("app_quit"),
};

/** Subscribes to a backend event; returns the unsubscribe function. */
export function on<T>(event: string, handler: (payload: T) => void): () => void {
  const p = listen<T>(event, (e) => handler(e.payload));
  return () => void p.then((un) => un());
}

export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return String(e);
}

/** A message's parts; older messages (and plain ones) derive them. */
export function partsOf(m: Message): Part[] {
  if (m.parts && m.parts.length) return m.parts;
  const out: Part[] = [];
  if (m.reasoning) out.push({ type: "reasoning", text: m.reasoning, ms: m.stats?.thinkMs ?? undefined });
  if (m.content) out.push({ type: "text", text: m.content });
  return out;
}

export function applyOps(parts: Part[], ops: Op[]): Part[] {
  const next = parts.slice();
  for (const op of ops) {
    if (op.op === "append") {
      const last = next[next.length - 1];
      if (last && last.type === op.kind) next[next.length - 1] = { ...last, text: last.text + op.text };
      else next.push(op.kind === "text" ? { type: "text", text: op.text } : { type: "reasoning", text: op.text });
    } else if (op.op === "tool") {
      const p = op.part;
      if (p.type !== "tool") continue;
      const i = next.findIndex((x) => x.type === "tool" && x.id === p.id);
      if (i >= 0) next[i] = p;
      else next.push(p);
    } else {
      const r = next[op.index];
      if (r && r.type === "reasoning") next[op.index] = { ...r, ms: op.ms };
    }
  }
  return next;
}

export const EFFORTS: { value: Effort; label: string; hint: string }[] = [
  { value: "off", label: "Off", hint: "Answers right away, without thinking" },
  { value: "low", label: "Low", hint: "A little thinking, faster answers" },
  { value: "medium", label: "Medium", hint: "Balanced" },
  { value: "high", label: "High", hint: "Thinks longer, for hard problems" },
  { value: "xhigh", label: "Extra high", hint: "Thinks much longer" },
  { value: "max", label: "Max", hint: "As long as it takes" },
];

/** Older versions had "auto": it reads as medium. */
export function effortOf(e: string | null | undefined): Effort {
  return EFFORTS.some((x) => x.value === e) ? (e as Effort) : "medium";
}
