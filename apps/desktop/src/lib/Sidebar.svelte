<script lang="ts">
  import Icon from "./icons/Icon.svelte";
  import Logo from "./icons/Logo.svelte";
  import { t, formatAgo, formatBytes } from "./i18n";
  import { L, setLang } from "./lang.svelte.js";
  import { history, clearHistory } from "./history.svelte.js";

  let {
    role,
    busy,
    version,
    now,
    onNav,
  }: { role: "send" | "receive"; busy: boolean; version: string; now: number; onNav: (r: "send" | "receive") => void } = $props();
</script>

<aside class="sidebar">
  <div class="brand"><Logo size={30} /><span>Peerly</span></div>

  <nav class="nav" aria-label="Peerly">
    <button class="navitem" class:active={role === "send"} disabled={busy} aria-current={role === "send" ? "page" : undefined} onclick={() => onNav("send")}>
      <Icon name="send" /><span>{t("send")}</span>
    </button>
    <button class="navitem" class:active={role === "receive"} disabled={busy} aria-current={role === "receive" ? "page" : undefined} onclick={() => onNav("receive")}>
      <Icon name="download" /><span>{t("receive")}</span>
    </button>
  </nav>

  <div class="recent">
    <div class="recent-head">
      <span class="label">{t("recent")}</span>
      {#if history.items.length}<button class="link small" onclick={clearHistory}>{t("clear")}</button>{/if}
    </div>
    {#if history.items.length === 0}
      <p class="muted small">{t("noRecent")}</p>
    {:else}
      <ul>
        {#each history.items as h}
          <li title={h.name}>
            <span class="dir" class:fail={!h.ok}><Icon name={h.dir === "up" ? "arrowUp" : "arrowDown"} size={14} /></span>
            <span class="meta">
              <span class="hname">{h.name}</span>
              <span class="hsub">
                {formatAgo(h.ts, now)} · {formatBytes(h.bytes)}{#if !h.ok} · <span class="failtxt">{t("failedLabel")}</span>{/if}
              </span>
            </span>
          </li>
        {/each}
      </ul>
    {/if}
  </div>

  <div class="foot">
    <div class="seg" role="group" aria-label={t("language")}>
      <button class:on={L.lang === "tr"} aria-pressed={L.lang === "tr"} onclick={() => setLang("tr")}>TR</button>
      <button class:on={L.lang === "en"} aria-pressed={L.lang === "en"} onclick={() => setLang("en")}>EN</button>
    </div>
    {#if version}<span class="muted small">v{version}</span>{/if}
  </div>
</aside>
