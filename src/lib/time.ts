import { useSyncExternalStore } from "react";

const DAY = 86_400_000;

function startOfDay(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

/** The sidebar's date groups, newest first. */
export function dateGroup(ms: number, now = new Date()): string {
  const today = startOfDay(now);
  if (ms >= today) return "Today";
  if (ms >= today - DAY) return "Yesterday";
  if (ms >= today - 7 * DAY) return "Previous 7 days";
  if (ms >= today - 30 * DAY) return "Previous 30 days";
  const d = new Date(ms);
  return d.getFullYear() === now.getFullYear()
    ? d.toLocaleDateString([], { month: "long" })
    : d.toLocaleDateString([], { month: "long", year: "numeric" });
}

/** "now", "5m", "14:20", "Tue", "Mar 3". */
export function shortWhen(ms: number, now = new Date()): string {
  const mins = Math.round((now.getTime() - ms) / 60_000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m`;
  const d = new Date(ms);
  if (ms >= startOfDay(now)) return d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  if (ms >= startOfDay(now) - 6 * DAY) return d.toLocaleDateString([], { weekday: "short" });
  return d.toLocaleDateString([], { month: "short", day: "numeric" });
}

export function seconds(ms: number): string {
  if (ms < 1000) return `${Math.max(0, Math.round(ms))} ms`;
  const s = ms / 1000;
  if (s < 10) return `${s.toFixed(1)} s`;
  if (s < 90) return `${Math.round(s)} s`;
  const m = Math.floor(s / 60);
  return `${m} min ${Math.round(s - m * 60)} s`;
}

/** When a message was sent, the way you'd say it: "Just now", "5 minutes ago",
 *  "3 hours ago" (the same day), then the date and time ("2 Nov, 14:05"). */
export function sentAgo(ms: number, now: number): string {
  const minutes = Math.floor((now - ms) / 60_000);
  if (minutes < 1) return "Just now";
  if (minutes < 60) return minutes === 1 ? "1 minute ago" : `${minutes} minutes ago`;
  if (ms >= startOfDay(new Date(now))) {
    const hours = Math.floor(minutes / 60);
    return hours === 1 ? "1 hour ago" : `${hours} hours ago`;
  }
  const d = new Date(ms);
  const sameYear = d.getFullYear() === new Date(now).getFullYear();
  const date = d.toLocaleDateString([], sameYear ? { day: "numeric", month: "short" } : { day: "numeric", month: "short", year: "numeric" });
  return `${date}, ${clock(d)}`;
}

/** The full date and time, for a tooltip. */
export function fullTime(ms: number): string {
  const d = new Date(ms);
  return `${d.toLocaleDateString([], { weekday: "long", day: "numeric", month: "long", year: "numeric" })}, ${clock(d)}`;
}

/** The hour in the system's own style (24-hour or AM/PM). */
function clock(d: Date): string {
  return d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

// One clock for every "minutes ago": it ticks on the minute while any is on screen.
let now = Date.now();
const listeners = new Set<() => void>();
let timer: ReturnType<typeof setTimeout> | null = null;

function tick() {
  timer = setTimeout(() => {
    now = Date.now();
    listeners.forEach((l) => l());
    tick();
  }, 60_000 - (Date.now() % 60_000) + 50);
}

function subscribe(l: () => void) {
  listeners.add(l);
  if (!timer) {
    now = Date.now();
    tick();
  }
  return () => {
    listeners.delete(l);
    if (!listeners.size && timer) {
      clearTimeout(timer);
      timer = null;
    }
  };
}

/** The current time, updated every minute. */
export function useNow(): number {
  return useSyncExternalStore(subscribe, () => now);
}
