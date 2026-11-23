import { motion } from "framer-motion";
import { Box, Check, ChevronDown, ChevronRight, RefreshCw, Search, Settings2 } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { EFFORTS, api, effortOf, errorText, type ModelRef, type Provider } from "../lib/api";
import { spring } from "../lib/motion";
import { isOffline, providerModels, shortModel, useChatOptions, useStore, useView } from "../lib/store";
import { IconGlyph, iconOf } from "./ProviderIcon";
import { MenuItem, Popover, SubmenuItem, anchorFrom, type Anchor } from "./ui";

/** The list of every provider's models, with a filter and arrow keys. Models
 *  that can't answer right now (their server is down, or stopped listing
 *  them) wait in one folded "Offline" section at the end. Used by the
 *  composer chip, "Try again with…" and the title model setting. `top` goes
 *  above the list (an extra choice). `footer` replaces "Manage models…",
 *  which then becomes a gear in the search row. */
export function ModelMenu({
  value,
  onPick,
  onClose,
  top,
  footer,
}: {
  value: ModelRef | null;
  onPick: (ref: ModelRef) => void;
  onClose: () => void;
  top?: React.ReactNode;
  footer?: React.ReactNode;
}) {
  const providers = useStore((s) => s.providers);
  const online = useStore((s) => s.online);
  const [filter, setFilter] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [cursor, setCursor] = useState(0);
  const list = useRef<HTMLDivElement>(null);
  const currentOffline = (() => {
    const p = providers.find((x) => x.id === value?.providerId);
    return !!p && !!value && isOffline(p, value.model, online);
  })();
  const [showOffline, setShowOffline] = useState(currentOffline);
  // In Agent mode, models that can't call tools carry a badge.
  const agent = useView() === "agent";
  const caps = useStore((s) => s.caps);

  useEffect(() => {
    void useStore.getState().probeProviders();
  }, []);

  useEffect(() => {
    if (!agent) return;
    const refs = providers.flatMap((p) => providerModels(p).map((model) => ({ providerId: p.id, model }))).slice(0, 40);
    for (const r of refs) useStore.getState().ensureCaps(r);
  }, [agent, providers]);

  const { groups, offline } = useMemo(() => {
    const f = filter.trim().toLowerCase();
    const gone: { p: Provider; model: string }[] = [];
    const live = providers
      .map((p) => {
        const all = providerModels(p).filter((m) => !f || m.toLowerCase().includes(f) || p.name.toLowerCase().includes(f));
        const models = all.filter((m) => {
          if (!isOffline(p, m, online)) return true;
          gone.push({ p, model: m });
          return false;
        });
        return { p, models, hidden: all.length - models.length };
      })
      // A provider whose models are all offline shows only in the Offline section.
      .filter((g) => g.models.length || (!f && !g.hidden));
    return { groups: live, offline: gone };
  }, [providers, filter, online]);
  // While filtering, matches in the Offline section count too.
  const offlineOpen = showOffline || (!!filter.trim() && offline.length > 0);
  const flat = useMemo(
    () => [
      ...groups.flatMap((g) => g.models.map((m) => ({ providerId: g.p.id, model: m }))),
      ...(offlineOpen ? offline.map((o) => ({ providerId: o.p.id, model: o.model })) : []),
    ],
    [groups, offline, offlineOpen],
  );

  useEffect(() => {
    // Start on the current model.
    const i = flat.findIndex((r) => r.providerId === value?.providerId && r.model === value?.model);
    setCursor(i >= 0 && !filter ? i : 0);
  }, [filter, flat, value]);

  useEffect(() => {
    list.current?.querySelector(".model-item.cursor")?.scrollIntoView({ block: "nearest" });
  }, [cursor]);

  const refresh = async (id: string) => {
    setBusy(id);
    try {
      const p = await api.refreshProvider(id);
      useStore.setState((s) => ({ providers: s.providers.map((x) => (x.id === id ? p : x)) }));
    } catch (e) {
      useStore.getState().toast(errorText(e));
    } finally {
      setBusy(null);
    }
  };

  const manage = () => {
    onClose();
    useStore.getState().openSettings("models");
  };

  let index = -1;
  return (
    <>
      <div className="model-search">
        <Search size={14} />
        <input
          autoFocus
          placeholder="Find a model"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setCursor((c) => Math.min(flat.length - 1, c + 1));
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setCursor((c) => Math.max(0, c - 1));
            } else if (e.key === "Enter" && flat[cursor]) {
              e.preventDefault();
              onPick(flat[cursor]);
            }
          }}
        />
        {footer && (
          <button className="model-manage" aria-label="Manage models" title="Manage models" onClick={manage}>
            <Settings2 size={14} />
          </button>
        )}
      </div>
      <div className="model-list" ref={list}>
        {top}
        {groups.map(({ p, models }) => (
          <div key={p.id} className="model-group">
            <div className="menu-section model-group-head">
              <span className="model-group-name">
                <span className="model-group-icon">
                  <IconGlyph icon={iconOf(p)} size={12} />
                </span>
                {p.name}
              </span>
              <button className={`model-refresh ${busy === p.id ? "spinning" : ""}`} aria-label={`Refresh ${p.name}'s models`} title="Refresh models" onClick={() => void refresh(p.id)}>
                <RefreshCw size={12} />
              </button>
            </div>
            {models.length === 0 && <div className="model-none">No models yet. Refresh, or add one in Settings.</div>}
            {models.map((m) => {
              index += 1;
              const i = index;
              const on = value?.providerId === p.id && value.model === m;
              return (
                <button
                  key={m}
                  className={`model-item ${on ? "on" : ""} ${i === cursor ? "cursor" : ""}`}
                  onMouseMove={() => i !== cursor && setCursor(i)}
                  onClick={() => onPick({ providerId: p.id, model: m })}
                >
                  <span className="model-item-text">
                    <span className="model-item-name">{shortModel(m)}</span>
                    {m.includes("/") && <span className="model-item-path">{m.slice(0, m.lastIndexOf("/"))}</span>}
                  </span>
                  {agent && caps[`${p.id}/${m}`]?.caps.tools === false && (
                    <span className="model-badge" title="This model can't call tools, so it can't work in a folder">
                      no tools
                    </span>
                  )}
                  {on && <Check size={14} className="menu-check" />}
                </button>
              );
            })}
          </div>
        ))}
        {offline.length > 0 && (
          <div className="model-group">
            <button className="menu-section model-offline-head" aria-expanded={offlineOpen} onClick={() => setShowOffline(!offlineOpen)}>
              <motion.span className="model-offline-chev" animate={{ rotate: offlineOpen ? 90 : 0 }} transition={spring}>
                <ChevronRight size={12} />
              </motion.span>
              <span>Offline</span>
              <span className="model-offline-count">{offline.length}</span>
            </button>
            {offlineOpen &&
              offline.map(({ p, model: m }) => {
                index += 1;
                const i = index;
                const on = value?.providerId === p.id && value.model === m;
                return (
                  <button
                    key={`${p.id}/${m}`}
                    className={`model-item offline ${on ? "on" : ""} ${i === cursor ? "cursor" : ""}`}
                    title={`${p.name} didn't answer, or no longer lists this model`}
                    onMouseMove={() => i !== cursor && setCursor(i)}
                    onClick={() => onPick({ providerId: p.id, model: m })}
                  >
                    <span className="model-item-text">
                      <span className="model-item-name">{shortModel(m)}</span>
                      <span className="model-item-path">{p.name}</span>
                    </span>
                    {on && <Check size={14} className="menu-check" />}
                  </button>
                );
              })}
          </div>
        )}
        {groups.length === 0 && offline.length === 0 && <div className="model-none">Nothing matches “{filter}”</div>}
      </div>
      <div className="menu-sep" />
      {footer ?? (
        <button className="menu-item" onClick={manage}>
          <span className="menu-icon">
            <Settings2 size={15} />
          </span>
          <span className="menu-label">Manage models…</span>
        </button>
      )}
    </>
  );
}

/** The composer's model chip: the model and its thinking effort in one
 *  borderless button (a box shows on hover). Its menu ends with the effort,
 *  in a side menu that opens on hover. */
export function ModelEffortPicker({ value }: { value: ModelRef | null }) {
  const providers = useStore((s) => s.providers);
  const opts = useChatOptions();
  const defaultEffort = useStore((s) => s.settings?.defaultEffort);
  const presets = useStore((s) => s.presets);
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const provider = providers.find((p) => p.id === value?.providerId);
  const close = () => setAnchor(null);
  const presetEffort = presets.find((p) => p.id === opts.presetId)?.effort ?? null;
  const effort = effortOf(opts.effort ?? presetEffort ?? defaultEffort);
  const effortLabel = EFFORTS.find((e) => e.value === effort)?.label ?? "Medium";
  return (
    <>
      <button
        ref={btn}
        className={`chip model-chip ${anchor ? "open" : ""}`}
        onClick={() => (anchor ? close() : setAnchor(anchorFrom(btn.current!, { above: true, alignRight: true })))}
        title={value ? `${provider?.name ?? ""} · ${value.model} · ${effortLabel} effort` : "Pick a model"}
      >
        <span className="chip-label">{value ? shortModel(value.model) : "Pick a model"}</span>
        {value && <span className="model-chip-effort">{effortLabel}</span>}
      </button>
      <Popover anchor={anchor} onClose={close} width={320} className="model-pop" trigger={btn}>
        <ModelMenu
          value={value}
          onClose={close}
          onPick={(ref) => {
            close();
            useStore.getState().pickModel(ref);
          }}
          footer={
            <SubmenuItem label="Effort" value={effortLabel} width={250}>
              {(done) =>
                EFFORTS.map((e) => (
                  <MenuItem
                    key={e.value}
                    checked={effort === e.value}
                    onSelect={() => {
                      done();
                      close();
                      useStore.getState().setOptions({ effort: e.value });
                    }}
                  >
                    <span className="menu-two">
                      <span>{e.label}</span>
                      <span className="menu-sub">{e.hint}</span>
                    </span>
                  </MenuItem>
                ))
              }
            </SubmenuItem>
          }
        />
      </Popover>
    </>
  );
}

export function ModelPicker({ value, above = true, onPick }: { value: ModelRef | null; above?: boolean; onPick?: (ref: ModelRef) => void }) {
  const providers = useStore((s) => s.providers);
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const provider = providers.find((p) => p.id === value?.providerId);
  const close = () => setAnchor(null);
  return (
    <>
      <button
        ref={btn}
        className={`chip ${anchor ? "open" : ""}`}
        onClick={() => (anchor ? close() : setAnchor(anchorFrom(btn.current!, { above })))}
        title={value ? `${provider?.name ?? ""} · ${value.model}` : "Pick a model"}
      >
        <Box size={14} className="chip-icon" />
        <span className="chip-label">{value ? shortModel(value.model) : "Pick a model"}</span>
        <ChevronDown size={13} className="chip-caret" />
      </button>
      <Popover anchor={anchor} onClose={close} width={320} className="model-pop" trigger={btn}>
        <ModelMenu
          value={value}
          onClose={close}
          onPick={(ref) => {
            close();
            (onPick ?? useStore.getState().pickModel)(ref);
          }}
        />
      </Popover>
    </>
  );
}

/** The model that names chats: "Same as the chat" or any model. */
export function TitleModelPicker({ value, onPick }: { value: ModelRef | null; onPick: (ref: ModelRef | null) => void }) {
  const providers = useStore((s) => s.providers);
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const close = () => setAnchor(null);
  // A model whose provider was removed falls back to the chat's (as Relay does).
  const ref = value && providers.some((p) => p.id === value.providerId) ? value : null;
  return (
    <>
      <button ref={btn} className={`chip ${anchor ? "open" : ""}`} onClick={() => (anchor ? close() : setAnchor(anchorFrom(btn.current!, { above: false })))}>
        <Box size={14} className="chip-icon" />
        <span className="chip-label">{ref ? shortModel(ref.model) : "Same as the chat"}</span>
        <ChevronDown size={13} className="chip-caret" />
      </button>
      <Popover anchor={anchor} onClose={close} width={320} className="model-pop" trigger={btn}>
        <ModelMenu
          value={ref}
          onClose={close}
          onPick={(r) => {
            close();
            onPick(r);
          }}
          top={
            <button
              className={`model-item ${ref ? "" : "on"}`}
              onClick={() => {
                close();
                onPick(null);
              }}
            >
              <span className="model-item-text">
                <span className="model-item-name">Same as the chat</span>
              </span>
              {!ref && <Check size={14} className="menu-check" />}
            </button>
          }
        />
      </Popover>
    </>
  );
}
