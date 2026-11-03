/** Local-only recent transfers. localStorage key `peerly.history.v1`: JSON array, newest first, max 20. */
export type HistoryEntry = {
  name: string;
  dir: "up" | "down";
  bytes: number;
  /** Unix ms when the transfer finished. */
  ts: number;
  ok: boolean;
};

const KEY = "peerly.history.v1";
const MAX = 20;

function load(): HistoryEntry[] {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    if (!Array.isArray(v)) return [];
    return v
      .filter((e) => e && typeof e.name === "string" && typeof e.ts === "number")
      .slice(0, MAX);
  } catch {
    return [];
  }
}

export const history = $state<{ items: HistoryEntry[] }>({ items: load() });

function save() {
  try {
    localStorage.setItem(KEY, JSON.stringify(history.items));
  } catch {
    /* ignore */
  }
}

export function addHistory(e: Omit<HistoryEntry, "ts">) {
  history.items = [{ ...e, ts: Date.now() }, ...history.items].slice(0, MAX);
  save();
}

export function clearHistory() {
  history.items = [];
  save();
}
