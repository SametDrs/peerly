<script lang="ts">
  import { onMount } from "svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { getVersion } from "@tauri-apps/api/app";
  import * as api from "./lib/api";
  import Icon from "./lib/icons/Icon.svelte";
  import Sidebar from "./lib/Sidebar.svelte";
  import { addHistory } from "./lib/history.svelte.js";
  import { ERR_CODES, PEER_HINT_CODES } from "./lib/i18n";
  import { t, formatBytes, formatDuration, type Key } from "./lib/i18n";

  type Screen = "pick" | "code" | "enter" | "offer" | "progress" | "done" | "error";
  type Role = "send" | "receive";

  let screen = $state<Screen>("pick");
  let role = $state<Role>("send");

  // send — `paths` and `dest` mirror the backend-owned selection (display only)
  let paths = $state<string[]>([]);
  /** Bumped on every selection change so a slow, stale inspection is ignored. */
  let inspectSeq = 0;
  let info = $state<{ files: number; bytes: number } | null>(null);
  let dragging = $state(false);
  let code = $state("");
  let copied = $state(false);
  // receive
  let codeInput = $state("");
  let dest = $state("");
  let connecting = $state(false);
  /** Sender: between "Send" and the code appearing (DHT lookup, a few seconds). */
  let preparing = $state(false);
  let offer = $state<{ name: string; files: number; total_bytes: number } | null>(null);
  // progress
  let done = $state(0);
  let total = $state(0);
  let speed = $state(0);
  let currentFile = $state("");
  let summary = $state({ files: 0, bytes: 0, elapsed_ms: 0 });
  let errorMsg = $state("");
  let errorCode = $state("other");
  let version = $state("");
  let now = $state(Date.now());
  /** Set once a transfer has started; used to write the local history entry. */
  let active: { name: string; dir: "up" | "down" } | null = null;
  let route = $state<"lan" | "internet" | "">("");
  const errKeys: Record<string, true> = Object.fromEntries(ERR_CODES.map((c) => [`err_${c}`, true]));

  const pct = $derived(total > 0 ? Math.min(100, (done / total) * 100) : 0);
  const eta = $derived(speed > 0 ? (total - done) / speed : NaN);
  const baseName = (p: string) => p.split(/[\\/]/).filter(Boolean).pop() ?? p;
  const busy = $derived((screen !== "pick" && screen !== "enter") || preparing || connecting);
  const routeLabel = $derived(route === "lan" ? t("routeLan") : route === "internet" ? t("routeInternet") : "");

  function recordHistory(ok: boolean, bytes: number) {
    if (!active) return;
    addHistory({ name: active.name, dir: active.dir, bytes, ok });
    active = null;
  }

  function reset() {
    screen = role === "send" ? "pick" : "enter";
    clearSelection();
    code = "";
    codeInput = "";
    offer = null;
    connecting = false;
    preparing = false;
    done = total = speed = 0;
    currentFile = "";
    errorMsg = "";
    route = "";
    active = null;
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
        preparing = false;
        screen = "code";
        break;
      case "connecting":
        connecting = true;
        break;
      case "connected":
        route = e.route === "lan" ? "lan" : e.route === "internet" ? "internet" : "";
        break;
      case "offer":
        offer = e;
        screen = "offer";
        break;
      case "started":
        active =
          role === "send"
            ? { name: paths.length > 1 ? `${baseName(paths[0])} +${paths.length - 1}` : baseName(paths[0] ?? ""), dir: "up" }
            : { name: offer?.name ?? "", dir: "down" };
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
        recordHistory(true, e.bytes);
        screen = "done";
        break;
      case "error":
        errorMsg = e.message;
        errorCode = e.code;
        recordHistory(false, total);
        preparing = false;
        screen = "error";
        break;
    }
  }

  /** Mirrors the backend selection (from a command result or `peerly://selection`). */
  async function applySelection(s: api.Selection) {
    dest = s.dest;
    const changed = s.paths.length !== paths.length || s.paths.some((p, i) => p !== paths[i]);
    if (!changed) return;
    paths = s.paths;
    const seq = ++inspectSeq;
    if (!paths.length) {
      info = null;
      return;
    }
    const r = await api.inspectSelection();
    if (seq === inspectSeq) info = r;
  }

  function clearSelection() {
    paths = [];
    info = null;
    inspectSeq++;
    api.clearSelection().then(applySelection).catch(() => {});
  }

  // Native dialogs are opened by the backend; the webview never handles paths itself.
  async function chooseFiles() {
    await applySelection(await api.pickFiles());
  }
  async function chooseFolder() {
    await applySelection(await api.pickFolder());
  }
  async function chooseDest() {
    await applySelection(await api.pickDestDir());
  }

  // OS drops are added to the selection by the backend, only while this is on.
  $effect(() => {
    api.setDropEnabled(role === "send" && screen === "pick" && !preparing).catch(() => {});
  });

  async function startSend() {
    role = "send";
    route = "";
    preparing = true;
    done = total = speed = 0;
    try {
      await api.startSend();
    } catch {
      /* error event already emitted */
    }
  }

  async function startReceive() {
    done = total = speed = 0;
    route = "";
    connecting = true;
    try {
      await api.startReceive(normalizeCode(codeInput));
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
    screen = role === "send" ? "pick" : "enter";
  }

  function chooseRole(r: Role) {
    if (busy) return;
    role = r;
    screen = r === "send" ? "pick" : "enter";
  }

  onMount(() => {
    const unsubs: Array<Promise<() => void>> = [];
    unsubs.push(api.onSelection(applySelection));
    api.getSelection().then(applySelection).catch(() => {});
    getVersion().then((v) => (version = v)).catch(() => {});
    const tick = setInterval(() => (now = Date.now()), 30000);
    unsubs.push(api.onPeerlyEvent(handleEvent));
    unsubs.push(
      // Highlight only: the dropped paths are taken by the backend from the OS
      // event and arrive via `peerly://selection`.
      getCurrentWebview().onDragDropEvent((ev) => {
        const p = ev.payload;
        const accepting = role === "send" && screen === "pick" && !preparing;
        if (p.type === "enter" || p.type === "over") dragging = accepting;
        else dragging = false;
      }),
    );
    return () => {
      clearInterval(tick);
      unsubs.forEach((u) => u.then((f) => f()));
    };
  });
</script>

<div class="shell">
  <Sidebar {role} {busy} {version} {now} onNav={chooseRole} />
  <main>
    <div class="content">
      {#if screen === "pick"}
        <header class="col" style="gap: 10px">
          <h1 class="title">{t("titleSend")}</h1>
          <p class="lead">{t("descSend")}</p>
        </header>
        <div class="chips">
          <span class="chip"><span class="ic"><Icon name="users" size={14} /></span>{t("chipNoAccount")}</span>
          <span class="chip"><span class="ic"><Icon name="shield" size={14} /></span>{t("chipE2E")}</span>
          <span class="chip"><span class="ic"><Icon name="bolt" size={14} /></span>{t("chipFast")}</span>
        </div>
        <div class="drop" class:active={dragging}>
          <span class="plus"><Icon name="plus" size={26} stroke={2.2} /></span>
          <b>{t("dropTitle")}</b>
          <span class="muted">{t("dropOr")}</span>
          <div class="btns">
            <button class="outline" onclick={chooseFiles} disabled={preparing}><Icon name="file" />{t("addFiles")}</button>
            <button class="outline" onclick={chooseFolder} disabled={preparing}><Icon name="folder" />{t("addFolder")}</button>
          </div>
        </div>
        {#if paths.length}
          <div class="files">
            <div class="summary">
              <b>{t("selected", { n: t("items", { n: paths.length }) })}{#if info} <span class="muted" style="font-weight: 400">· {t("filesCount", { n: info.files })} · {formatBytes(info.bytes)}</span>{/if}</b>
              <button class="link small" disabled={preparing} onclick={clearSelection}>{t("clear")}</button>
            </div>
            {#each paths.slice(0, 4) as p}
              <div class="item"><Icon name="file" size={16} /><span class="path">{baseName(p)}</span></div>
            {/each}
            {#if paths.length > 4}<div class="muted small" style="padding-left: 4px">+{paths.length - 4}</div>{/if}
          </div>
        {/if}
        <div class="actions">
          {#if preparing}
            <span class="status"><span class="spinner"></span>{t("preparing")}</span>
            <button class="danger" onclick={goBackAndCancel}>{t("cancel")}</button>
          {:else}
            {#if !paths.length}<span class="hint"><Icon name="info" size={15} />{t("sendHint")}</span>{/if}
            <button class="primary with-icon" disabled={!paths.length} onclick={startSend}><Icon name="send" />{t("sendBtn")}</button>
          {/if}
        </div>
      {:else if screen === "code"}
        <header class="col" style="gap: 10px">
          <h1 class="title">{t("waitingTitle")}</h1>
          <p class="lead">{t("shareCode")}</p>
        </header>
        <div class="banner">
          <div class="body">
            <span class="label">{t("yourCode")}</span>
            <div class="code">{code}</div>
          </div>
          <button class="outline" onclick={copyCode}><Icon name={copied ? "check" : "copy"} />{copied ? t("copied") : t("copy")}</button>
          <button class="iconbtn" onclick={goBackAndCancel} aria-label={t("cancelAria")} title={t("cancel")}><Icon name="x" /></button>
        </div>
        <p class="lead">{t("waitingHint")}</p>
        <div class="actions" style="justify-content: center">
          <span class="status"><span class="pulse"></span>{t("waiting")}</span>
        </div>
      {:else if screen === "enter"}
        <header class="col" style="gap: 10px">
          <h1 class="title">{t("titleReceive")}</h1>
          <p class="lead">{t("descReceive")}</p>
        </header>
        <label class="col" style="gap: 8px">
          <span class="muted">{t("enterCode")}</span>
          <span class="inputwrap">
            <Icon name="key" size={22} />
            <!-- svelte-ignore a11y_autofocus -->
            <input
              class="codeinput"
              autofocus
              autocomplete="off"
              autocapitalize="off"
              spellcheck="false"
              placeholder={t("codePlaceholder")}
              bind:value={codeInput}
              onpaste={(e) => {
                const txt = e.clipboardData?.getData("text");
                if (txt) {
                  e.preventDefault();
                  codeInput = normalizeCode(txt);
                }
              }}
              disabled={connecting}
              onkeydown={(e) => e.key === "Enter" && codeInput.trim() && !connecting && startReceive()}
            />
          </span>
        </label>
        <div class="card destrow">
          <Icon name="folder" size={22} />
          <div class="grow">
            <span class="muted small">{t("saveTo")}</span>
            <div class="path">{dest}</div>
          </div>
          <button class="outline" onclick={chooseDest} disabled={connecting}>{t("change")}</button>
        </div>
        <div class="actions" style="flex-direction: column; align-items: stretch; gap: 12px">
          {#if connecting}
            <span class="status"><span class="spinner"></span>{t("connecting")}</span>
            <p class="muted center">{t("connectingHint")}</p>
            <button class="danger" onclick={goBackAndCancel}>{t("cancel")}</button>
          {:else}
            <button class="primary with-icon" style="align-self: flex-end" disabled={!codeInput.trim()} onclick={startReceive}><Icon name="download" />{t("connect")}</button>
          {/if}
        </div>
      {:else if screen === "offer" && offer}
        <div class="vcenter">
          {#if routeLabel}<p class="center"><span class="badge">{routeLabel}</span></p>{/if}
          <p class="center muted">{t("from")}</p>
          <div class="card col center">
            <h2>{offer.name}</h2>
            <p class="muted">{t("filesCount", { n: offer.files })} · {formatBytes(offer.total_bytes)}</p>
            <p class="path center">{dest}</p>
          </div>
        </div>
        <div class="row">
          <button onclick={async () => { await api.respondOffer(false); reset(); }}>{t("decline")}</button>
          <button class="primary" onclick={() => api.respondOffer(true)}>{t("accept")}</button>
        </div>
      {:else if screen === "progress"}
        <div class="vcenter">
          {#if routeLabel}<p class="center"><span class="badge">{routeLabel}</span></p>{/if}
          <p class="center muted">{role === "send" ? t("sending") : t("receiving")}</p>
          <div class="big">{pct.toFixed(0)}%</div>
          <div class="bar"><div style="width: {pct}%"></div></div>
          <div class="stats">
            <div><b>{formatBytes(done)} / {formatBytes(total)}</b><span>&nbsp;</span></div>
            <div><b>{(speed / 1e6).toFixed(1)} MB/s</b><span>{t("speed")}</span></div>
            <div><b>{formatDuration(eta)}</b><span>{t("eta")}</span></div>
          </div>
          <div class="file">{currentFile}</div>
        </div>
        <button class="danger" onclick={goBackAndCancel}>{t("cancel")}</button>
      {:else if screen === "done"}
        <div class="vcenter">
          <div class="hero"><Icon name="check" size={32} stroke={2.4} /></div>
          <h2 class="center">{t("done")}</h2>
          <p class="muted center">
            {t(role === "send" ? "sentSummary" : "receivedSummary", {
              files: summary.files,
              size: formatBytes(summary.bytes),
              time: formatDuration(summary.elapsed_ms / 1000),
            })}
          </p>
        </div>
        <div class="row">
          {#if role === "receive"}
            <button class="outline" onclick={() => api.openFolder()}><Icon name="folderOpen" />{t("openFolder")}</button>
          {/if}
          <button class="primary" onclick={reset}>{t(role === "send" ? "sendAnother" : "receiveAnother")}</button>
        </div>
      {:else if screen === "error"}
        <div class="vcenter">
          <div class="hero bad"><Icon name="alert" size={30} /></div>
          <h2 class="center err">{t("errorTitle")}</h2>
          <p class="center">{friendly(errorCode)}</p>
          {#if PEER_HINT_CODES.includes(errorCode)}<p class="center muted">{t("hintPeer")}</p>{/if}
          <details>
            <summary>{t("details")}</summary>
            <p>{errorMsg}</p>
          </details>
        </div>
        <div class="row">
          <button onclick={reset}>{t("home")}</button>
          <button class="primary" onclick={retry}>{t("tryAgain")}</button>
        </div>
      {/if}
    </div>
  </main>
</div>
