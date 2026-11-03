export type Lang = "tr" | "en";
const KEY = "peerly.lang";

function initial(): Lang {
  try {
    const s = localStorage.getItem(KEY);
    if (s === "tr" || s === "en") return s;
  } catch {
    /* storage unavailable */
  }
  return (navigator.language || "en").toLowerCase().startsWith("tr") ? "tr" : "en";
}

/** Reactive UI language; read it inside templates (via `t()`) to re-render on change. */
export const L = $state<{ lang: Lang }>({ lang: initial() });

export function setLang(l: Lang) {
  L.lang = l;
  try {
    localStorage.setItem(KEY, l);
  } catch {
    /* ignore */
  }
}
