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
  /** How long Anthropic keeps a conversation's prompt cache. */
  anthropicCache: "5m" | "1h";
  /** The switches a new chat starts with: the last ones used. */
  chatSkills: boolean;
  chatMcp: boolean;
  chatWeb: boolean;
  /** What the window shows: chats, or Agent sessions. */
  view: ChatKind;
  /** The mode a new Agent session starts with (the last one used). */
  agentMode: AgentMode;
}

/** A chat, or an Agent session (a chat that works in a folder). */
export type ChatKind = "chat" | "agent";

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
/** Relay's own web search tools (web.rs). */
export const WEB_SERVER = "relay.web";
/** Agent sessions' own tools (files, commands). */
export const AGENT_SERVER = "relay.agent";

/** The answer to a tool call waiting for approval ("edits": allow, and switch the session to Accept edits). */
export type ToolDecision = "once" | "always" | "deny" | "edits";

/** An Agent session's permission mode. */
export type AgentMode = "ask" | "edits" | "auto" | "bypass";
/** A step Auto refused, kept in the session's blocked list. */
export interface BlockedStep {
  tool: string;
  args: Record<string, unknown>;
  rule: string;
  why: string;
  mode: AgentMode;
  at: number;
  messageId: string;
  callId: string;
  ran: boolean;
  result?: string;
  error?: boolean;
}
/** A folder's Auto settings, and the secret environment variables Relay has. */
export interface FolderSettings {
  autoPush: boolean;
  secrets: { name: string; allowed: boolean }[];
}
/** The server's own tools, run inside its reply (Anthropic's web search). */
export const HOSTED_SERVER = "anthropic.hosted";

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
  /** The protocol the server speaks. */
  api: ProviderApi;
}

export type ProviderApi = "openai" | "anthropic";

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
  /** MCP tools. */
  tools: boolean;
  skills: boolean;
  web: boolean;
  /** In the sidebar's Archived group. */
  archived: boolean;
  kind: ChatKind;
  /** An Agent session's folder (null until its first message when none was picked). */
  folder: string | null;
  /** The folder is a workspace Relay made for the session. */
  folderOwned: boolean;
  /** An Agent session's permission mode; null: the default. */
  mode: AgentMode | null;
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
  /** x/thumb.png exists: the square tiles show. */
  thumb?: boolean;
  /** PDFs: how many pages. */
  pageCount?: number;
  /** PDFs without text: how many pages go as images (x/page-N.jpg). */
  pages: number;
}

export type Ability = "vision" | "audio" | "video" | "pdf" | "tools";
/** What a model takes; `tools` is null until it's known (said by the server, or checked). */
export type Caps = Record<Exclude<Ability, "tools">, boolean> & { tools: boolean | null };

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

export type ToolStatus = "approval" | "question" | "running" | "done" | "error" | "denied" | "blocked";

/** A question the model asks (agent__ask_user). */
export interface Question {
  question: string;
  header: string;
  options: { label: string; description: string }[];
  multiple: boolean;
}

/** One answer: the labels picked, and anything typed under "Other". */
export interface Answer {
  picked: string[];
  other: string;
}

/** One diff line: "+" | "-" | " ", its old and new line numbers, its text. */
export type DiffRow = [string, number | null, number | null, string];

export interface Diff {
  hunks: DiffRow[][];
  added: number;
  removed: number;
  truncated: boolean;
}

/** What an agent tool's card shows besides its result. */
export interface ToolDetail {
  path?: string;
  diff?: Diff;
  created?: boolean;
  /** Why an approval is worth a second look (outside the folder, credentials). */
  note?: string;
  blocked?: { rule: string; why: string };
  folder?: boolean;
  files?: number;
  /** The checkpoint taken before this step ("Restore to before this"). */
  checkpoint?: number;
  /** Why no checkpoint could be taken. */
  unprotected?: string;
  /** Commands: the shell, the output's end (live while it runs), how it ended. */
  shell?: string;
  output?: string;
  exit?: number | null;
  timedOut?: boolean;
  /** A command left running in the background. */
  process?: number;
  /** agent__ask_user: the questions, and the answers once given (null: closed). */
  questions?: Question[];
  /** A session's summary (the agent's `summarize` part). */
  summary?: string;
  keepUser?: boolean;
  answers?: Answer[] | null;
  /** agent__todo: the list. */
  items?: { text: string; status: "pending" | "in_progress" | "done" }[];
}

export interface ProcessView {
  id: number;
  command: string;
  shell: string;
  started: number;
  running: boolean;
  code: number | null;
}

export interface Checkpoint {
  id: number;
  chatId: string;
  label: string;
  createdAt: number;
  /** False: pruned to save space. */
  restorable: boolean;
}

export interface Restored {
  written: number;
  removed: number;
  /** Restoring to this undoes the restore. */
  undo: number;
}

/** What a session changed in its folder. */
export interface Changes {
  files: { path: string; kind: "created" | "changed" | "deleted"; versions: number }[];
  /** Files too big for checkpoints: [path, size]. */
  big: [string, number][];
  /** The session changed nothing yet. */
  none: boolean;
}

export interface FileVersion {
  hash: string;
  size: number;
  at: number;
  checkpoint: number;
}

export type Part =
  | { type: "reasoning"; text: string; ms?: number }
  | { type: "text"; text: string }
  | { type: "tool"; id: string; serverId: string; server: string; tool: string; arguments: string; status: ToolStatus; result?: string; ms?: number; detail?: ToolDetail };

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
  /** MCP tools. */
  tools?: boolean;
  skills?: boolean;
  web?: boolean;
  /** Only for a new chat. */
  kind?: ChatKind;
  /** Only for a new Agent session: the folder picked for it (none: a workspace). */
  folder?: string | null;
  mode?: AgentMode | null;
}

export interface SearchHit {
  chatId: string;
  title: string;
  updatedAt: number;
  snippet: string;
  archived: boolean;
  kind: ChatKind;
}

export interface FolderInfo {
  name: string;
  exists: boolean;
  /** The git branch (or a short commit), in a repository. */
  branch: string | null;
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
  api?: ProviderApi;
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
  testProvider: (baseUrl: string, key?: string, id?: string, kind?: ProviderApi) => invoke<string[]>("provider_test", { baseUrl, key, id, kind }),
  refreshProvider: (id: string) => invoke<Provider>("provider_refresh", { id }),
  modelContext: (providerId: string, model: string) => invoke<number | null>("model_context", { providerId, model }),
  modelCaps: (providerId: string, model: string) => invoke<ModelCaps | null>("model_caps", { providerId, model }),
  /** Asks the model whether it calls tools (one small request). */
  checkTools: (providerId: string, model: string) => invoke<ModelCaps | null>("model_check_tools", { providerId, model }),
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
  attachInfo: (id: string, info: { duration?: number; width?: number; height?: number; pageCount?: number }) => invoke<Attachment>("attach_info", { id, ...info }),
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
  decideTool: (messageId: string, callId: string, decision: ToolDecision) => invoke<void>("tool_decide", { messageId, callId, decision }),

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
  /** `deleteFolder`: also the workspace Relay made for an Agent session. */
  deleteChat: (id: string, deleteFolder = false, deleteCheckpoints = false) => invoke<void>("chat_delete", { id, deleteFolder, deleteCheckpoints }),
  deleteAllChats: () => invoke<void>("chats_delete_all"),
  setChatModel: (id: string, providerId: string, model: string) => invoke<void>("chat_set_model", { id, providerId, model }),
  setChatOptions: (id: string, options: ChatOptions) => invoke<ChatSummary | null>("chat_set_options", { id, options }),
  send: (args: { chatId: string | null; text: string; attachments: string[]; providerId: string; model: string; options?: ChatOptions }) =>
    invoke<{ chat: ChatSummary; user: Message | null; assistant: Message }>("chat_send", args),
  edit: (chatId: string, messageId: string, text: string, attachments: string[], providerId: string, model: string) =>
    invoke<Chat>("chat_edit", { chatId, messageId, text, attachments, providerId, model }),
  regenerate: (chatId: string, messageId: string, providerId: string, model: string) =>
    invoke<Chat>("chat_regenerate", { chatId, messageId, providerId, model }),
  /** Continue: a reply that didn't finish goes on in the same message. */
  continueReply: (chatId: string, messageId: string) => invoke<Chat>("chat_continue", { chatId, messageId }),
  branch: (chatId: string, messageId: string) => invoke<Chat>("chat_branch", { chatId, messageId }),
  stop: (chatId: string) => invoke<void>("chat_stop", { chatId }),
  streamingChats: () => invoke<string[]>("streaming_chats"),
  search: (query: string, kind: ChatKind) => invoke<SearchHit[]>("chat_search", { query, kind }),
  folderInfo: (path: string) => invoke<FolderInfo>("agent_folder_info", { path }),
  openSessionFolder: (chatId: string) => invoke<void>("agent_open_folder", { chatId }),
  /** Reads the folder's AGENTS.md / CLAUDE.md again; returns how long it is (0: none). */
  reloadInstructions: (chatId: string) => invoke<number>("agent_reload_instructions", { chatId }),
  checkpoints: (chatId: string) => invoke<Checkpoint[]>("agent_checkpoints", { chatId }),
  processes: (chatId: string) => invoke<ProcessView[]>("agent_processes", { chatId }),
  blocked: (chatId: string) => invoke<BlockedStep[]>("agent_blocked", { chatId }),
  runBlocked: (chatId: string, at: number) => invoke<BlockedStep>("agent_run_blocked", { chatId, at }),
  clearBlocked: (chatId: string) => invoke<void>("agent_clear_blocked", { chatId }),
  folderSettings: (folder: string) => invoke<FolderSettings>("agent_folder_settings", { folder }),
  setFolderSettings: (folder: string, change: { autoPush?: boolean; secret?: string; allow?: boolean }) => invoke<FolderSettings>("agent_set_folder_settings", { folder, ...change }),
  /** Answers to the model's questions (null: closed without answering). */
  answerTool: (messageId: string, callId: string, answers: Answer[] | null) => invoke<void>("tool_answer", { messageId, callId, answers }),
  stopProcess: (chatId: string, id: number) => invoke<boolean>("agent_process_stop", { chatId, id }),
  restore: (chatId: string, checkpoint: number) => invoke<Restored>("agent_restore", { chatId, checkpoint }),
  sessionChanges: (chatId: string) => invoke<Changes>("agent_changes", { chatId }),
  fileVersions: (chatId: string, path: string) => invoke<FileVersion[]>("agent_file_versions", { chatId, path }),
  versionDiff: (chatId: string, path: string, hash: string) => invoke<Diff>("agent_version_diff", { chatId, path, hash }),
  restoreFile: (chatId: string, path: string, hash: string) => invoke<number>("agent_restore_file", { chatId, path, hash }),
  openSessionFile: (chatId: string, path: string) => invoke<void>("agent_open_file", { chatId, path }),
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
