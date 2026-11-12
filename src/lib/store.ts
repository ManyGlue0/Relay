import { useEffect } from "react";
import { create } from "zustand";
import {
  api,
  applyOps,
  errorText,
  partsOf,
  type Ability,
  type Attachment,
  type Chat,
  type ChatOptions,
  type ChatSummary,
  type Effort,
  type McpStatus,
  type McpView,
  type Message,
  type ModelCaps,
  type ModelRef,
  type Preset,
  type Provider,
  type ProviderStatus,
  type SearchHit,
  type Settings,
  type Skill,
  type StreamDelta,
} from "./api";
import { prepare } from "./media";

export type SettingsSection = "general" | "you" | "models" | "presets" | "tools" | "skills" | "stats" | "about";

export interface Toast {
  id: number;
  text: string;
  action?: { label: string; run: () => void };
}

/** A file in the composer: still being copied and prepared, or ready to send. */
export interface DraftFile {
  key: string;
  name: string;
  ready: boolean;
  /** There as soon as it's copied (thumbnails show while Relay prepares the rest). */
  attachment?: Attachment;
  /** What a long preparation is at ("Page 3 of 40"). */
  progress?: string;
}

/** Files a message may carry. */
const MAX_FILES = 20;

interface State {
  loaded: boolean;
  settings: Settings | null;
  providers: Provider[];
  presets: Preset[];
  mcp: McpView[];
  skills: Skill[];
  chats: ChatSummary[];
  /** The open chat; null while composing a new one. */
  currentId: string | null;
  current: Chat | null;
  /** Chats with a reply in progress. */
  streaming: Record<string, true>;
  /** What a new chat will use (before it has an id). */
  draftModel: ModelRef | null;
  draftOptions: ChatOptions;
  query: string;
  hits: SearchHit[] | null;
  /** Settings take the window while open. */
  settingsOpen: boolean;
  section: SettingsSection;
  /** An editor open inside a section ("new" for a fresh item). */
  editing: string | null;
  toasts: Toast[];
  systemDark: boolean;
  maximized: boolean;
  windowActive: boolean;
  /** Bumped to ask the composer to take focus. */
  focusComposer: number;
  /** Context window per "provider/model", once asked. */
  contexts: Record<string, number | null>;
  /** Which providers answered at the last check (the model picker's Offline section). */
  online: Record<string, ProviderStatus>;
  /** What each "provider/model" takes besides text, once asked. */
  caps: Record<string, ModelCaps | null>;
  /** Files in the composer, per chat ("new" for a new one), like the text drafts. */
  files: Record<string, DraftFile[]>;

  init(): Promise<void>;
  /** Asks every provider whether it's there; at most every few seconds. */
  probeProviders(): Promise<void>;
  refreshChats(): Promise<void>;
  refreshProviders(): Promise<void>;
  refreshPresets(): Promise<void>;
  refreshMcp(): Promise<void>;
  refreshSkills(): Promise<void>;
  openChat(id: string): Promise<void>;
  newChat(): void;
  send(text: string): Promise<boolean>;
  /** `keep`: the ids of the original's attachments that stay. */
  edit(messageId: string, text: string, keep: string[]): Promise<boolean>;
  regenerate(messageId: string, model?: ModelRef): Promise<void>;
  branch(messageId: string): Promise<void>;
  stop(): void;
  decideTool(messageId: string, callId: string, decision: "once" | "always" | "deny"): Promise<void>;
  renameChat(id: string, title: string): Promise<void>;
  pinChat(id: string, pinned: boolean): Promise<void>;
  archiveChat(id: string, archived: boolean): Promise<void>;
  /** Adds files to the composer of the chat on screen: paths (picked, dropped) or pasted files. */
  attach(src: { paths?: string[]; files?: File[] }): Promise<void>;
  /** A voice message, recorded in Relay. */
  attachVoice(wav: Uint8Array, seconds: number): Promise<void>;
  detach(key: string): void;
  /** Opens an attachment in the app Windows uses for it. */
  openAttachment(id: string): Promise<void>;
  ensureCaps(ref: ModelRef): void;
  setCap(ref: ModelRef, ability: Ability, on: boolean | null): Promise<void>;
  deleteChat(id: string): Promise<void>;
  pickModel(ref: ModelRef): void;
  setOptions(opts: ChatOptions): void;
  applyDelta(d: StreamDelta): void;
  reloadCurrent(): Promise<void>;
  applyEnd(m: Message): void;
  applyChat(c: ChatSummary): void;
  applyMcpStatus(st: McpStatus): void;
  updateSettings(patch: Partial<Settings>): Promise<void>;
  openSettings(section?: SettingsSection, editing?: string | null): void;
  closeSettings(): void;
  setQuery(q: string): void;
  ensureContext(ref: ModelRef): void;
  toast(text: string, action?: Toast["action"]): void;
  dismissToast(id: number): void;
}

let toastId = 0;
let searchSeq = 0;
/** Batches that arrived before the message they belong to. */
const early: Record<string, StreamDelta[]> = {};

export const useStore = create<State>((set, get) => ({
  loaded: false,
  settings: null,
  providers: [],
  presets: [],
  mcp: [],
  skills: [],
  chats: [],
  currentId: null,
  current: null,
  streaming: {},
  draftModel: null,
  draftOptions: {},
  query: "",
  hits: null,
  settingsOpen: false,
  section: "general",
  editing: null,
  toasts: [],
  systemDark: window.matchMedia("(prefers-color-scheme: dark)").matches,
  maximized: false,
  windowActive: document.hasFocus(),
  focusComposer: 0,
  contexts: {},
  online: {},
  caps: {},
  files: {},

  async probeProviders() {
    const now = Date.now();
    if (now - lastProbe < 4000) return;
    lastProbe = now;
    try {
      const { statuses, changed } = await api.probeProviders();
      set({ online: Object.fromEntries(statuses.map((s) => [s.id, s])) });
      if (changed) await get().refreshProviders();
    } catch {
      /* a failed check changes nothing */
    }
  },

  async init() {
    const [settings, providers, presets, mcp, skills, chats, streaming] = await Promise.all([
      api.getSettings(),
      api.providers(),
      api.presets(),
      api.mcp(),
      api.skills(),
      api.chats(),
      api.streamingChats(),
    ]);
    set({
      settings,
      providers,
      presets,
      mcp,
      skills,
      chats,
      draftOptions: { presetId: settings.defaultPreset },
      streaming: Object.fromEntries(streaming.map((id) => [id, true] as const)),
      loaded: true,
    });
    void get().probeProviders();
  },

  async refreshChats() {
    set({ chats: await api.chats() });
  },
  async refreshProviders() {
    set({ providers: await api.providers() });
  },
  async refreshPresets() {
    set({ presets: await api.presets() });
  },
  async refreshMcp() {
    set({ mcp: await api.mcp() });
  },
  async refreshSkills() {
    // Skill flags live in settings too.
    const [skills, settings] = await Promise.all([api.skills(), api.getSettings()]);
    set({ skills, settings });
  },

  async openChat(id) {
    set({ settingsOpen: false });
    if (get().currentId === id && get().current) {
      set((s) => ({ focusComposer: s.focusComposer + 1 }));
      return;
    }
    set({ currentId: id });
    try {
      const chat = await api.chat(id);
      if (get().currentId !== id) return;
      if (!chat) {
        set({ currentId: null, current: null });
        get().toast("That chat no longer exists");
        void get().refreshChats();
        return;
      }
      set((s) => ({ current: chat, focusComposer: s.focusComposer + 1 }));
      if (get().windowActive) void api.holsterSeen(id);
    } catch (e) {
      get().toast(errorText(e));
    }
  },

  newChat() {
    const s = get();
    set({
      currentId: null,
      current: null,
      draftModel: null,
      draftOptions: { presetId: s.settings?.defaultPreset ?? null },
      settingsOpen: false,
      focusComposer: s.focusComposer + 1,
    });
  },

  async send(text) {
    const st = get();
    const model = activeModel(st);
    if (!model) {
      st.toast("Pick a model first");
      return false;
    }
    const chatId = st.currentId;
    const draftKey = chatId ?? "new";
    const files = st.files[draftKey] ?? [];
    if (files.some((f) => !f.ready)) {
      st.toast("Wait a moment: a file is still being prepared");
      return false;
    }
    const attachments = files.map((f) => f.attachment!.id);
    try {
      const res = await api.send({ chatId, text, attachments, providerId: model.providerId, model: model.model, options: chatId ? undefined : st.draftOptions });
      set((s) => {
        const chats = [res.chat, ...s.chats.filter((c) => c.id !== res.chat.id)];
        const streaming = { ...s.streaming, [res.chat.id]: true as const };
        const left = { ...s.files };
        delete left[draftKey];
        // Still looking at the chat we sent from (or the draft that became it)?
        if (s.currentId !== chatId) return { chats, streaming, files: left };
        const base: Chat = s.current && s.current.id === res.chat.id ? s.current : { ...res.chat, systemPrompt: "", leafId: null, messages: [] };
        const added = [res.user, { ...res.assistant, thinking: true }].filter(Boolean) as Message[];
        return {
          chats,
          streaming,
          files: left,
          currentId: res.chat.id,
          current: { ...base, ...res.chat, leafId: res.assistant.id, messages: [...base.messages, ...added] },
        };
      });
      flushEarly(res.assistant.id);
      if (!chatId) void api.updateSettings({ defaultModel: model }).then((settings) => set({ settings }));
      return true;
    } catch (e) {
      st.toast(errorText(e));
      return false;
    }
  },

  async edit(messageId, text, keep) {
    const st = get();
    const model = activeModel(st);
    if (!model || !st.current) return false;
    try {
      const chat = await api.edit(st.current.id, messageId, text, keep, model.providerId, model.model);
      set((s) => ({ current: chat, chats: s.chats.map((c) => (c.id === chat.id ? { ...c, archived: chat.archived } : c)), streaming: { ...s.streaming, [chat.id]: true } }));
      flushEarly(chat.leafId ?? "");
      return true;
    } catch (e) {
      st.toast(errorText(e));
      return false;
    }
  },

  async regenerate(messageId, ref) {
    const st = get();
    const model = ref ?? activeModel(st);
    if (!model || !st.current) return;
    try {
      if (ref) st.pickModel(ref);
      const chat = await api.regenerate(st.current.id, messageId, model.providerId, model.model);
      set((s) => ({ current: chat, chats: s.chats.map((c) => (c.id === chat.id ? { ...c, archived: chat.archived } : c)), streaming: { ...s.streaming, [chat.id]: true } }));
      flushEarly(chat.leafId ?? "");
    } catch (e) {
      st.toast(errorText(e));
    }
  },

  async branch(messageId) {
    const st = get();
    if (!st.current) return;
    try {
      const chat = await api.branch(st.current.id, messageId);
      if (get().currentId === chat.id) set({ current: chat });
    } catch (e) {
      st.toast(errorText(e));
    }
  },

  stop() {
    const id = get().currentId;
    if (id) void api.stop(id);
  },

  async decideTool(messageId, callId, decision) {
    try {
      await api.decideTool(messageId, callId, decision);
      if (decision === "always") void get().refreshMcp();
    } catch (e) {
      get().toast(errorText(e));
    }
  },

  async renameChat(id, title) {
    try {
      await api.renameChat(id, title);
      set((s) => ({
        chats: s.chats.map((c) => (c.id === id ? { ...c, title: title.trim() } : c)),
        current: s.current?.id === id ? { ...s.current, title: title.trim() } : s.current,
      }));
    } catch (e) {
      get().toast(errorText(e));
    }
  },

  async pinChat(id, pinned) {
    await api.pinChat(id, pinned);
    set((s) => ({ chats: sortChats(s.chats.map((c) => (c.id === id ? { ...c, pinned } : c))) }));
  },

  async archiveChat(id, archived) {
    try {
      await api.archiveChat(id, archived);
      set((s) => ({
        chats: s.chats.map((c) => (c.id === id ? { ...c, archived } : c)),
        current: s.current?.id === id ? { ...s.current, archived } : s.current,
        hits: s.hits?.map((h) => (h.chatId === id ? { ...h, archived } : h)) ?? null,
      }));
      if (archived) get().toast("Chat archived", { label: "Undo", run: () => void get().archiveChat(id, false) });
    } catch (e) {
      get().toast(errorText(e));
    }
  },

  async attach(src) {
    const key = get().currentId ?? "new";
    const jobs: { key: string; name: string; add: () => Promise<Attachment> }[] = [];
    for (const path of src.paths ?? []) {
      jobs.push({ key: tempKey(), name: path.split(/[\\/]/).pop() || "file", add: () => api.attachPath(path) });
    }
    for (const f of src.files ?? []) {
      // Screenshots paste as "image.png".
      const name = !f.name || f.name === "image.png" ? `Pasted image.${f.type.split("/")[1] || "png"}` : f.name;
      jobs.push({ key: tempKey(), name, add: async () => api.attachBytes(name, new Uint8Array(await f.arrayBuffer())) });
    }
    const room = MAX_FILES - (get().files[key]?.length ?? 0);
    if (jobs.length > room) {
      get().toast(`A message can carry ${MAX_FILES} files at most`);
      jobs.splice(Math.max(0, room));
    }
    if (!jobs.length) return;
    set((s) => ({ files: { ...s.files, [key]: [...(s.files[key] ?? []), ...jobs.map((j) => ({ key: j.key, name: j.name, ready: false }))] } }));
    await Promise.all(
      jobs.map(async (j) => {
        try {
          const a = await j.add();
          patchFile(key, j.key, { attachment: a, name: a.name });
          patchFile(key, j.key, { attachment: await prepare(a, (progress) => patchFile(key, j.key, { progress })), ready: true });
        } catch (e) {
          dropFile(key, j.key);
          get().toast(errorText(e));
        }
      }),
    );
  },

  async attachVoice(wav, seconds) {
    const key = get().currentId ?? "new";
    const tmp = tempKey();
    set((s) => ({ files: { ...s.files, [key]: [...(s.files[key] ?? []), { key: tmp, name: "Voice message", ready: false }] } }));
    try {
      const a = await api.attachBytes("Voice message.wav", wav, { voice: true, duration: seconds });
      patchFile(key, tmp, { attachment: a, ready: true });
    } catch (e) {
      dropFile(key, tmp);
      get().toast(errorText(e));
    }
  },

  detach(fileKey) {
    const key = get().currentId ?? "new";
    const f = get().files[key]?.find((x) => x.key === fileKey);
    dropFile(key, fileKey);
    if (f?.attachment) void api.attachDiscard([f.attachment.id]).catch(() => {});
  },

  async openAttachment(id) {
    try {
      await api.attachOpen(id);
    } catch (e) {
      get().toast(errorText(e));
    }
  },

  ensureCaps(ref) {
    const key = `${ref.providerId}/${ref.model}`;
    if (key in get().caps) return;
    set((s) => ({ caps: { ...s.caps, [key]: null } }));
    void api
      .modelCaps(ref.providerId, ref.model)
      .then((c) => set((s) => ({ caps: { ...s.caps, [key]: c } })))
      .catch(() => {});
  },

  async setCap(ref, ability, on) {
    try {
      const c = await api.setModelCap(ref.providerId, ref.model, ability, on);
      set((s) => ({ caps: { ...s.caps, [`${ref.providerId}/${ref.model}`]: c } }));
    } catch (e) {
      get().toast(errorText(e));
    }
  },

  async deleteChat(id) {
    try {
      await api.deleteChat(id);
      set((s) => {
        const streaming = { ...s.streaming };
        delete streaming[id];
        const leaving = s.currentId === id;
        return {
          chats: s.chats.filter((c) => c.id !== id),
          hits: s.hits?.filter((h) => h.chatId !== id) ?? null,
          streaming,
          ...(leaving ? { currentId: null, current: null } : {}),
        };
      });
    } catch (e) {
      get().toast(errorText(e));
    }
  },

  pickModel(ref) {
    const st = get();
    st.ensureContext(ref);
    if (st.current) {
      const id = st.current.id;
      void api.setChatModel(id, ref.providerId, ref.model).catch((e) => st.toast(errorText(e)));
      set((s) => ({
        current: s.current ? { ...s.current, providerId: ref.providerId, model: ref.model } : null,
        chats: s.chats.map((c) => (c.id === id ? { ...c, providerId: ref.providerId, model: ref.model } : c)),
      }));
    } else {
      set({ draftModel: ref });
      void api.updateSettings({ defaultModel: ref }).then((settings) => set({ settings }));
    }
  },

  setOptions(opts) {
    const st = get();
    if (!st.current) {
      set((s) => ({ draftOptions: { ...s.draftOptions, ...opts } }));
      return;
    }
    const id = st.current.id;
    // The backend reads "" as "clear".
    const wire: ChatOptions = {
      presetId: opts.presetId === null ? "" : opts.presetId,
      effort: opts.effort === null ? ("" as Effort) : opts.effort,
      tools: opts.tools,
    };
    set((s) => ({ current: s.current ? { ...s.current, ...opts } : null }));
    void api
      .setChatOptions(id, wire)
      .then((summary) => summary && get().applyChat(summary))
      .catch((e) => st.toast(errorText(e)));
  },

  applyDelta(d) {
    let missing = false;
    let reload = false;
    set((s) => {
      const streaming = s.streaming[d.chatId] ? s.streaming : { ...s.streaming, [d.chatId]: true as const };
      const cur = s.current;
      const i = cur && cur.id === d.chatId ? cur.messages.findIndex((m) => m.id === d.messageId) : -1;
      if (!cur || i < 0) {
        // A reply whose message we don't have yet (the send is still coming back).
        missing = true;
        return streaming === s.streaming ? s : { streaming };
      }
      const m = cur.messages[i];
      if (d.seq <= m.seq) return { streaming }; // already in what we loaded
      if (d.seq > m.seq + 1) {
        reload = true; // a batch went missing: load the chat again
        return { streaming };
      }
      const messages = cur.messages.slice();
      messages[i] = { ...m, parts: applyOps(partsOf(m), d.ops), thinking: d.thinking, seq: d.seq };
      return { streaming, current: { ...cur, messages } };
    });
    if (missing) {
      const list = (early[d.messageId] ??= []);
      list.push(d);
      setTimeout(() => delete early[d.messageId], 10_000);
    }
    if (reload) void get().reloadCurrent();
  },

  async reloadCurrent() {
    const id = get().currentId;
    if (!id) return;
    const chat = await api.chat(id);
    if (chat && get().currentId === id) set({ current: chat });
  },

  applyEnd(msg) {
    set((s) => {
      const streaming = { ...s.streaming };
      delete streaming[msg.chatId];
      const chats = sortChats(s.chats.map((c) => (c.id === msg.chatId ? { ...c, updatedAt: Date.now() } : c)));
      const cur = s.current;
      if (!cur || cur.id !== msg.chatId) return { streaming, chats };
      const messages = cur.messages.map((m) => (m.id === msg.id ? msg : m));
      return { streaming, chats, current: { ...cur, messages } };
    });
  },

  applyChat(c) {
    set((s) => ({
      chats: s.chats.map((x) => (x.id === c.id ? c : x)),
      current: s.current?.id === c.id ? { ...s.current, ...c } : s.current,
    }));
  },

  applyMcpStatus(st) {
    set((s) => ({ mcp: s.mcp.map((m) => (m.id === st.id ? { ...m, status: st } : m)) }));
  },

  async updateSettings(patch) {
    const prev = get().settings;
    if (prev) set({ settings: { ...prev, ...patch } });
    try {
      set({ settings: await api.updateSettings(patch) });
    } catch (e) {
      if (prev) set({ settings: prev });
      get().toast(errorText(e));
    }
  },

  openSettings(section = "general", editing = null) {
    set({ settingsOpen: true, section, editing });
  },

  closeSettings() {
    set((s) => ({ settingsOpen: false, editing: null, focusComposer: s.focusComposer + 1 }));
  },

  setQuery(query) {
    set({ query });
    const seq = ++searchSeq;
    if (!query.trim()) {
      set({ hits: null });
      return;
    }
    void api.search(query).then((hits) => {
      if (seq === searchSeq) set({ hits });
    });
  },

  ensureContext(ref) {
    const key = `${ref.providerId}/${ref.model}`;
    if (key in get().contexts) return;
    set((s) => ({ contexts: { ...s.contexts, [key]: null } }));
    void api
      .modelContext(ref.providerId, ref.model)
      .then((n) => set((s) => ({ contexts: { ...s.contexts, [key]: n } })))
      .catch(() => {});
  },

  toast(text, action) {
    const id = ++toastId;
    set((s) => ({ toasts: [...s.toasts.slice(-2), { id, text, action }] }));
    setTimeout(() => get().dismissToast(id), action ? 6000 : 4000);
  },

  dismissToast(id) {
    set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) }));
  },
}));

let tempSeq = 0;
const tempKey = () => `f${++tempSeq}`;

function patchFile(chatKey: string, fileKey: string, patch: Partial<DraftFile>) {
  useStore.setState((s) => {
    const list = s.files[chatKey];
    if (!list?.some((f) => f.key === fileKey)) return {};
    return { files: { ...s.files, [chatKey]: list.map((f) => (f.key === fileKey ? { ...f, ...patch } : f)) } };
  });
}

function dropFile(chatKey: string, fileKey: string) {
  useStore.setState((s) => {
    const list = (s.files[chatKey] ?? []).filter((f) => f.key !== fileKey);
    const files = { ...s.files, [chatKey]: list };
    if (!list.length) delete files[chatKey];
    return { files };
  });
}

function flushEarly(messageId: string) {
  const list = early[messageId];
  if (!list) return;
  delete early[messageId];
  for (const d of list) useStore.getState().applyDelta(d);
}

function sortChats(chats: ChatSummary[]): ChatSummary[] {
  return chats.slice().sort((a, b) => Number(b.pinned) - Number(a.pinned) || b.updatedAt - a.updatedAt);
}

/** Every model a provider offers: listed ones, then the ones typed in. */
/** The models a provider shows: all it lists plus the ones added, or (with
 *  "show every model" off) only the ones picked or typed. */
export function providerModels(p: Provider): string[] {
  const seen = new Set<string>();
  // Picked models keep the server's order; typed ones follow.
  const listed = p.showAll ? p.models : p.models.filter((m) => p.customModels.includes(m));
  return [...listed, ...p.customModels].filter((m) => (seen.has(m) ? false : (seen.add(m), true)));
}

/** A model that can't answer right now: its server didn't respond at the last
 *  check, or no longer lists it. Unknown counts as available. */
export function isOffline(p: Provider, model: string, online: Record<string, ProviderStatus>): boolean {
  const st = online[p.id];
  if (!st) return false;
  if (!st.online) return true;
  return !!st.models && st.models.length > 0 && !st.models.includes(model);
}

let lastProbe = 0;

function valid(ref: ModelRef | null | undefined, providers: Provider[]): ref is ModelRef {
  return !!ref && providers.some((p) => p.id === ref.providerId);
}

/** The model the composer sends with: the chat's, else the draft's, else the
 *  default, else the first one available. */
export function activeModel(s: Pick<State, "current" | "draftModel" | "settings" | "providers">): ModelRef | null {
  if (s.current?.providerId && s.current.model) {
    const ref = { providerId: s.current.providerId, model: s.current.model };
    if (valid(ref, s.providers)) return ref;
  }
  if (valid(s.draftModel, s.providers)) return s.draftModel;
  if (valid(s.settings?.defaultModel, s.providers)) return s.settings!.defaultModel;
  for (const p of s.providers) {
    const m = providerModels(p)[0];
    if (m) return { providerId: p.id, model: m };
  }
  return null;
}

export function useActiveModel(): ModelRef | null {
  const current = useStore((s) => s.current);
  const draftModel = useStore((s) => s.draftModel);
  const settings = useStore((s) => s.settings);
  const providers = useStore((s) => s.providers);
  return activeModel({ current, draftModel, settings, providers });
}

/** The options the composer shows: the chat's, or the draft's. */
export function useChatOptions(): { presetId: string | null; effort: Effort | null; tools: boolean } {
  const current = useStore((s) => s.current);
  const draft = useStore((s) => s.draftOptions);
  if (current) return { presetId: current.presetId, effort: current.effort, tools: current.tools };
  return { presetId: draft.presetId ?? null, effort: draft.effort ?? null, tools: draft.tools ?? true };
}

/** "qwen3:8b" from "library/qwen3:8b"; ids without a path stay as they are. */
export function shortModel(id: string): string {
  const i = id.lastIndexOf("/");
  return i >= 0 && i < id.length - 1 ? id.slice(i + 1) : id;
}

export function compact(n: number): string {
  if (n < 1000) return String(n);
  if (n < 10_000) return `${(n / 1000).toFixed(1)}k`;
  if (n < 1_000_000) return `${Math.round(n / 1000)}k`;
  return `${(n / 1_000_000).toFixed(n < 10_000_000 ? 1 : 0)}M`;
}

// ---- hooks --------------------------------------------------------------------------

export function useDark(): boolean {
  const theme = useStore((s) => s.settings?.theme ?? "system");
  const systemDark = useStore((s) => s.systemDark);
  return theme === "dark" || (theme === "system" && systemDark);
}

/** Escape handling, innermost overlay first. Handlers register while open. */
const escapeStack: (() => void)[] = [];

export function useEscape(active: boolean, onEscape: () => void) {
  useEffect(() => {
    if (!active) return;
    escapeStack.push(onEscape);
    return () => {
      const i = escapeStack.lastIndexOf(onEscape);
      if (i >= 0) escapeStack.splice(i, 1);
    };
  }, [active, onEscape]);
}

export function handleEscape(): boolean {
  const top = escapeStack[escapeStack.length - 1];
  if (!top) return false;
  top();
  return true;
}
