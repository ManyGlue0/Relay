import { AnimatePresence, motion } from "framer-motion";
import { BarChart3 } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { api, type Usage } from "../../lib/api";
import { compact, shortModel, useStore } from "../../lib/store";
import { seconds } from "../../lib/time";
import { Empty, Segmented } from "../ui";
import { Group, Page } from "./kit";

type Range = "7" | "30" | "90" | "all";

export function Stats() {
  const [range, setRange] = useState<Range>("30");
  const [usage, setUsage] = useState<Usage | null>(null);
  useEffect(() => {
    void api.usage(range === "all" ? null : Number(range)).then(setUsage);
  }, [range]);

  const picker = (
    <Segmented
      id="range"
      value={range}
      onChange={setRange}
      options={[
        { value: "7", label: "7 days" },
        { value: "30", label: "30 days" },
        { value: "90", label: "90 days" },
        { value: "all", label: "All time" },
      ]}
    />
  );

  if (usage && usage.replies === 0) {
    return (
      <Page title="Stats">
        <Group>
          <div className="group-empty">
            <Empty icon={<BarChart3 size={22} />} title="Nothing to count yet">
              Once you chat, Relay shows how much you use each model and how fast it answers.
            </Empty>
          </div>
        </Group>
      </Page>
    );
  }

  return (
    <Page title="Stats" action={picker}>
      <div className="tiles">
        <Tile label="Chats" value={usage ? usage.chats.toLocaleString() : "…"} />
        <Tile label="Replies" value={usage ? usage.replies.toLocaleString() : "…"} />
        <Tile label="Tokens written" value={usage ? compact(usage.completionTokens) : "…"} hint="By the models" />
        <Tile label="Tool calls" value={usage ? usage.toolCalls.toLocaleString() : "…"} />
      </div>
      {usage && <DailyChart days={usage.days} range={range} />}
      {usage && usage.models.length > 0 && <ModelTable usage={usage} />}
    </Page>
  );
}

function Tile({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <div className="tile">
      <div className="tile-label">{label}</div>
      <div className="tile-value">{value}</div>
      {hint && <div className="tile-hint">{hint}</div>}
    </div>
  );
}

function localDay(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

type Bucket = { key: string; start: Date; tokens: number; replies: number };

function addDays(d: Date, n: number): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate() + n);
}

/** The usage as bars: one a day for short spans, one a week up to two years, then one a month. */
function buckets(days: Usage["days"], range: Range): { list: Bucket[]; unit: "day" | "week" | "month" } {
  const today = new Date();
  const byDay = new Map(days.map((d) => [d.day, d]));
  let first: Date;
  if (range === "all") {
    const earliest = days[0]?.day;
    first = earliest ? new Date(`${earliest}T00:00:00`) : addDays(today, -29);
    // At least a month, so a new install doesn't draw one fat bar.
    if (today.getTime() - first.getTime() < 29 * 86_400_000) first = addDays(today, -29);
  } else {
    first = addDays(today, -(Number(range) - 1));
  }
  const span = Math.round((today.getTime() - first.getTime()) / 86_400_000) + 1;
  const unit = span <= 120 ? "day" : span <= 730 ? "week" : "month";
  const keyOf = (d: Date) =>
    unit === "day" ? localDay(d) : unit === "week" ? localDay(addDays(d, -((d.getDay() + 6) % 7))) : `${d.getFullYear()}-${d.getMonth()}`;
  const list: Bucket[] = [];
  for (let d = first; d <= today; d = addDays(d, 1)) {
    const k = keyOf(d);
    let b = list[list.length - 1];
    if (!b || b.key !== k) {
      b = { key: k, start: d, tokens: 0, replies: 0 };
      list.push(b);
    }
    const v = byDay.get(localDay(d));
    if (v) {
      b.tokens += v.tokens;
      b.replies += v.replies;
    }
  }
  return { list, unit };
}

/** Tokens written over time: one hue, bars anchored to the baseline, hover for the numbers. */
function DailyChart({ days, range }: { days: Usage["days"]; range: Range }) {
  const [hover, setHover] = useState<number | null>(null);
  const { list: series, unit } = useMemo(() => buckets(days, range), [days, range]);
  const max = Math.max(1, ...series.map((s) => s.tokens));
  const nice = niceMax(max);
  const W = 640;
  const H = 168;
  const pad = { l: 36, r: 8, t: 10, b: 22 };
  const plotW = W - pad.l - pad.r;
  const plotH = H - pad.t - pad.b;
  const slot = plotW / series.length;
  const gap = Math.min(4, slot * 0.25);
  const bw = Math.max(2, slot - gap);
  const ticks = [0, nice / 2, nice];
  const labelEvery = Math.max(1, Math.ceil(series.length / 6));
  const h = hover != null ? series[hover] : null;
  const fmt = (d: Date) =>
    unit === "month" ? d.toLocaleDateString([], { month: "short", year: "2-digit" }) : d.toLocaleDateString([], { month: "short", day: "numeric" });
  const tipTitle = (b: Bucket) =>
    unit === "day"
      ? b.start.toLocaleDateString([], { weekday: "short", month: "short", day: "numeric" })
      : unit === "week"
        ? `Week of ${b.start.toLocaleDateString([], { month: "short", day: "numeric" })}`
        : b.start.toLocaleDateString([], { month: "long", year: "numeric" });

  return (
    <Group title={unit === "day" ? "Tokens written per day" : unit === "week" ? "Tokens written per week" : "Tokens written per month"}>
      <div className="chart" onMouseLeave={() => setHover(null)}>
        <svg viewBox={`0 0 ${W} ${H}`} width="100%" role="img" aria-label="Tokens written over time">
          {ticks.map((t) => {
            const y = pad.t + plotH - (t / nice) * plotH;
            return (
              <g key={t}>
                <line x1={pad.l} x2={W - pad.r} y1={y} y2={y} className={t === 0 ? "chart-base" : "chart-grid"} />
                <text x={pad.l - 6} y={y + 3.5} className="chart-tick" textAnchor="end">
                  {compact(t)}
                </text>
              </g>
            );
          })}
          {series.map((s, i) => {
            const bh = (s.tokens / nice) * plotH;
            const x = pad.l + i * slot + gap / 2;
            const y = pad.t + plotH - bh;
            const r = Math.min(4, bw / 2, bh);
            return (
              <g key={s.key} onMouseEnter={() => setHover(i)}>
                {/* The hit target is the whole slot, taller than the bar. */}
                <rect x={pad.l + i * slot} y={pad.t} width={slot} height={plotH} fill="transparent" />
                {s.tokens > 0 && (
                  <path
                    d={`M${x},${pad.t + plotH} V${y + r} Q${x},${y} ${x + r},${y} H${x + bw - r} Q${x + bw},${y} ${x + bw},${y + r} V${pad.t + plotH} Z`}
                    className={`chart-bar ${hover === i ? "on" : hover != null ? "dim" : ""}`}
                  />
                )}
                {(series.length - 1 - i) % labelEvery === 0 && (
                  <text x={pad.l + i * slot + slot / 2} y={H - 6} className="chart-tick" textAnchor="middle">
                    {fmt(s.start)}
                  </text>
                )}
              </g>
            );
          })}
        </svg>
        <AnimatePresence>
          {h && (
            <motion.div
              className="chart-tip"
              style={{ left: `${((pad.l + (hover! + 0.5) * slot) / W) * 100}%` }}
              initial={{ opacity: 0, y: 4 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, transition: { duration: 0.08 } }}
              transition={{ duration: 0.12 }}
            >
              <div className="chart-tip-title">{tipTitle(h)}</div>
              <div>
                {h.tokens.toLocaleString()} tokens · {h.replies === 1 ? "1 reply" : `${h.replies} replies`}
              </div>
            </motion.div>
          )}
        </AnimatePresence>
      </div>
    </Group>
  );
}

function niceMax(v: number): number {
  const p = Math.pow(10, Math.floor(Math.log10(v)));
  for (const m of [1, 2, 2.5, 5, 10]) if (m * p >= v) return m * p;
  return 10 * p;
}

function ModelTable({ usage }: { usage: Usage }) {
  const providers = useStore((s) => s.providers);
  return (
    <Group title="By model">
      <div className="table-scroll">
        <table className="stats-table">
          <thead>
            <tr>
              <th>Model</th>
              <th className="num">Replies</th>
              <th className="num">Tokens written</th>
              <th className="num">Speed</th>
              <th className="num">First token</th>
              <th className="num">Tool calls</th>
            </tr>
          </thead>
          <tbody>
            {usage.models.map((m) => (
              <tr key={`${m.providerId}/${m.model}`}>
                <td>
                  <div className="cell-model">{shortModel(m.model)}</div>
                  <div className="cell-sub">{providers.find((p) => p.id === m.providerId)?.name ?? "Removed provider"}</div>
                </td>
                <td className="num">
                  {m.replies.toLocaleString()}
                  {m.errors > 0 && <div className="cell-sub">{m.errors} failed</div>}
                </td>
                <td className="num">{compact(m.completionTokens)}</td>
                <td className="num">{m.avgTokensPerSec ? `${m.avgTokensPerSec.toFixed(m.avgTokensPerSec < 10 ? 1 : 0)} tok/s` : "–"}</td>
                <td className="num">{m.avgTtftMs != null ? seconds(m.avgTtftMs) : "–"}</td>
                <td className="num">{m.toolCalls.toLocaleString()}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </Group>
  );
}
