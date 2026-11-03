<script lang="ts">
  import { onMount } from "svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import { openPath } from "@tauri-apps/plugin-opener";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import * as api from "./lib/api";
  import { ERR_CODES } from "./lib/i18n";
  import { t, formatBytes, formatDuration, type Key } from "./lib/i18n";

  type Screen = "home" | "pick" | "code" | "enter" | "offer" | "progress" | "done" | "error";
  type Role = "send" | "receive";

  let screen = $state<Screen>("home");
  let role = $state<Role>("send");

  // send
  let paths = $state<string[]>([]);
  let info = $state<{ files: number; bytes: number } | null>(null);
  let dragging = $state(false);
  let code = $state("");
  let copied = $state(false);
  // receive
  let codeInput = $state("");
  let dest = $state("");
  let connecting = $state(false);
  let offer = $state<{ name: string; files: number; total_bytes: number } | null>(null);
  // progress
  let done = $state(0);
  let total = $state(0);
  let speed = $state(0);
  let currentFile = $state("");
  let summary = $state({ files: 0, bytes: 0, elapsed_ms: 0 });
  let errorMsg = $state("");
  let errorCode = $state("other");
  const errKeys: Record<string, true> = Object.fromEntries(ERR_CODES.map((c) => [`err_${c}`, true]));

  const pct = $derived(total > 0 ? Math.min(100, (done / total) * 100) : 0);
  const eta = $derived(speed > 0 ? (total - done) / speed : NaN);
  const baseName = (p: string) => p.split(/[\\/]/).filter(Boolean).pop() ?? p;

  function reset() {
    screen = "home";
    paths = [];
    info = null;
    code = "";
    codeInput = "";
    offer = null;
    connecting = false;
    done = total = speed = 0;
    currentFile = "";
    errorMsg = "";
  }

  async function goBackAndCancel() {
    await api.cancel();
    reset();
  }

  function normalizeCode(s: string) {
    return s.trim().toLowerCase().replace(/[\s_.,]+/g, "-").replace(/-+/g, "-");
  }

  function friendly(c: string): string {
    const key = `err_${c}`;
    return (key in errKeys ? t(key as Key) : t("err_other"));
  }

  function handleEvent(e: api.PeerlyEvent) {
    switch (e.kind) {
      case "code":
        code = e.code;
        screen = "code";
        break;
      case "connecting":
        connecting = true;
        break;
      case "offer":
        offer = e;
        screen = "offer";
        break;
      case "started":
        total = e.total_bytes;
        done = 0;
        screen = "progress";
        break;
      case "progress":
        done = e.bytes_done;
        total = e.bytes_total;
        speed = e.bytes_per_sec;
        break;
      case "file_completed":
        currentFile = baseName(e.path);
        break;
      case "completed":
        summary = e;
        screen = "done";
        break;
      case "error":
        errorMsg = e.message;
        errorCode = e.code;
        screen = "error";
        break;
    }
  }

  async function addPaths(p: string[]) {
    paths = [...new Set([...paths, ...p])];
    screen = "pick";
    info = await api.inspectPaths(paths);
  }

  async function chooseFiles() {
    const r = await open({ multiple: true, directory: false });
    if (r) await addPaths(Array.isArray(r) ? r : [r]);
  }
  async function chooseFolder() {
    const r = await open({ directory: true });
    if (typeof r === "string") await addPaths([r]);
  }
  async function chooseDest() {
    const r = await open({ directory: true, defaultPath: dest });
    if (typeof r === "string") dest = r;
  }

  async function startSend() {
    role = "send";
    done = total = speed = 0;
    try {
      await api.startSend(paths);
    } catch {
      /* error event already emitted */
    }
  }

  async function startReceive() {
    done = total = speed = 0;
    connecting = true;
    try {
      await api.startReceive(normalizeCode(codeInput), dest);
    } catch {
      /* error event already emitted */
    }
  }

  async function copyCode() {
    await navigator.clipboard.writeText(code);
    copied = true;
    setTimeout(() => (copied = false), 1500);
  }

  function retry() {
    errorMsg = "";
    connecting = false;
    offer = null;
    screen = role === "send" ? (paths.length ? "pick" : "home") : "enter";
  }

  function chooseRole(r: Role) {
    role = r;
    screen = r === "send" ? "pick" : "enter";
  }

  onMount(() => {
    const unsubs: Array<Promise<() => void>> = [];
    api.defaultDownloadDir().then((d) => (dest = d));
    unsubs.push(api.onPeerlyEvent(handleEvent));
    unsubs.push(
      getCurrentWebview().onDragDropEvent((ev) => {
        const p = ev.payload;
        const accepting = screen === "home" || screen === "pick";
        if (p.type === "enter" || p.type === "over") dragging = accepting;
        else if (p.type === "leave") dragging = false;
        else if (p.type === "drop") {
          dragging = false;
          if (accepting && p.paths.length) {
            role = "send";
            addPaths(p.paths);
          }
        }
      }),
    );
    return () => unsubs.forEach((u) => u.then((f) => f()));
  });
</script>

<main>
  {#if screen === "home"}
    <div class="center col" style="padding-top: 8px">
      <h1>Peerly</h1>
      <p class="muted">{t("tagline")}</p>
    </div>
    <div class="choices">
      <button class="choice primary" onclick={() => chooseRole("send")}>
        <span class="title">{t("send")}</span><span class="desc">{t("sendDesc")}</span>
      </button>
      <button class="choice" onclick={() => chooseRole("receive")}>
        <span class="title">{t("receive")}</span><span class="desc">{t("receiveDesc")}</span>
      </button>
    </div>
  {:else if screen === "pick"}
    <button class="link" onclick={reset}>← {t("back")}</button>
    <h2>{t("send")}</h2>
    <div class="drop" class:active={dragging}>
      <p>{t("dropHere")}</p>
    </div>
    <div class="row">
      <button onclick={chooseFiles}>{t("chooseFiles")}</button>
      <button onclick={chooseFolder}>{t("chooseFolder")}</button>
    </div>
    {#if paths.length}
      <div class="card col" style="gap: 8px">
        <div class="row" style="justify-content: space-between">
          <b style="flex: none">{t("selected", { n: t("items", { n: paths.length }) })}</b>
          <button class="link" style="flex: none; text-align: right" onclick={() => { paths = []; info = null; }}>{t("clear")}</button>
        </div>
        {#each paths.slice(0, 4) as p}
          <div class="path">{baseName(p)}</div>
        {/each}
        {#if paths.length > 4}<div class="muted">+{paths.length - 4}</div>{/if}
        {#if info}
          <div class="muted">{t("filesCount", { n: info.files })} · {formatBytes(info.bytes)}</div>
        {/if}
      </div>
    {/if}
    <div class="grow"></div>
    <button class="primary" disabled={!paths.length} onclick={startSend}>{t("start")}</button>
  {:else if screen === "code"}
    <h2>{t("yourCode")}</h2>
    <div class="card"><div class="code">{code}</div></div>
    <button onclick={copyCode}>{copied ? t("copied") : t("copy")}</button>
    <p class="muted center">{t("shareCode")}</p>
    <div class="grow"></div>
    <p class="center"><span class="pulse"></span>{t("waiting")}</p>
    <button class="danger" onclick={goBackAndCancel}>{t("cancel")}</button>
  {:else if screen === "enter"}
    <button class="link" onclick={reset}>← {t("back")}</button>
    <h2>{t("receive")}</h2>
    <label class="col" style="gap: 8px">
      <span class="muted">{t("enterCode")}</span>
      <!-- svelte-ignore a11y_autofocus -->
      <input
        class="codeinput"
        autofocus
        autocomplete="off"
        autocapitalize="off"
        spellcheck="false"
        placeholder={t("codePlaceholder")}
        bind:value={codeInput}
        disabled={connecting}
        onkeydown={(e) => e.key === "Enter" && codeInput.trim() && !connecting && startReceive()}
      />
    </label>
    <div class="card col" style="gap: 6px">
      <span class="muted">{t("saveTo")}</span>
      <div class="path">{dest}</div>
      <button class="link" onclick={chooseDest} disabled={connecting}>{t("change")}</button>
    </div>
    <div class="grow"></div>
    {#if connecting}
      <p class="center"><span class="pulse"></span>{t("connecting")}</p>
      <button class="danger" onclick={goBackAndCancel}>{t("cancel")}</button>
    {:else}
      <button class="primary" disabled={!codeInput.trim()} onclick={startReceive}>{t("connect")}</button>
    {/if}
  {:else if screen === "offer" && offer}
    <div class="grow"></div>
    <p class="center muted">{t("from")}</p>
    <div class="card col center">
      <h2>{offer.name}</h2>
      <p class="muted">{t("filesCount", { n: offer.files })} · {formatBytes(offer.total_bytes)}</p>
      <p class="path center">{dest}</p>
    </div>
    <div class="grow"></div>
    <div class="row">
      <button onclick={async () => { await api.respondOffer(false); reset(); }}>{t("decline")}</button>
      <button class="primary" onclick={() => api.respondOffer(true)}>{t("accept")}</button>
    </div>
  {:else if screen === "progress"}
    <div class="grow"></div>
    <p class="center muted">{role === "send" ? t("sending") : t("receiving")}</p>
    <div class="big">{pct.toFixed(0)}%</div>
    <div class="bar"><div style="width: {pct}%"></div></div>
    <div class="stats">
      <div><b>{formatBytes(done)} / {formatBytes(total)}</b><span>&nbsp;</span></div>
      <div><b>{(speed / 1e6).toFixed(1)} MB/s</b><span>{t("speed")}</span></div>
      <div><b>{formatDuration(eta)}</b><span>{t("eta")}</span></div>
    </div>
    <div class="file">{currentFile}</div>
    <div class="grow"></div>
    <button class="danger" onclick={goBackAndCancel}>{t("cancel")}</button>
  {:else if screen === "done"}
    <div class="grow"></div>
    <div class="check">✓</div>
    <h2 class="center">{t("done")}</h2>
    <p class="muted center">
      {t(role === "send" ? "sentSummary" : "receivedSummary", {
        files: summary.files,
        size: formatBytes(summary.bytes),
        time: formatDuration(summary.elapsed_ms / 1000),
      })}
    </p>
    <div class="grow"></div>
    {#if role === "receive"}
      <button onclick={() => openPath(dest)}>{t("openFolder")}</button>
    {/if}
    <button class="primary" onclick={reset}>{t(role === "send" ? "sendAnother" : "receiveAnother")}</button>
  {:else if screen === "error"}
    <div class="grow"></div>
    <h2 class="center err">{t("errorTitle")}</h2>
    <p class="center">{friendly(errorCode)}</p>
    <details>
      <summary>{t("details")}</summary>
      <p style="user-select: text">{errorMsg}</p>
    </details>
    <div class="grow"></div>
    <div class="row">
      <button onclick={reset}>{t("home")}</button>
      <button class="primary" onclick={retry}>{t("tryAgain")}</button>
    </div>
  {/if}
</main>
