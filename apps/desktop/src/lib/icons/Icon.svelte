<script lang="ts" module>
  export type IconName =
    | "send" | "download" | "plus" | "file" | "folder" | "key" | "shield" | "bolt" | "users"
    | "x" | "copy" | "check" | "arrowUp" | "arrowDown" | "info" | "alert" | "folderOpen";

  /** Small lucide-style stroke icons (hand-written paths, 24x24 grid). */
  const PATHS: Record<IconName, string[]> = {
    send: ["M22 2 11 13", "M22 2 15 22l-4-9-9-4 20-7z"],
    download: ["M12 3v12", "m7 10 5 5 5-5", "M5 21h14"],
    plus: ["M12 5v14", "M5 12h14"],
    file: ["M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z", "M14 2v6h6"],
    folder: ["M3 6a2 2 0 0 1 2-2h4l2 3h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"],
    folderOpen: ["M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v1", "M3 19l2-8h17l-2 8a2 2 0 0 1-2 1H5a2 2 0 0 1-2-1z"],
    key: ["M21 2l-9.6 9.6", "M15.5 7.5 18 10", "M7.5 10.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11z"],
    shield: ["M12 2 4 5v6c0 5 3.4 9.3 8 11 4.6-1.7 8-6 8-11V5z", "m9 12 2 2 4-4"],
    bolt: ["M13 2 3 14h9l-1 8 10-12h-9z"],
    users: ["M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2", "M9 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8z", "M22 21v-2a4 4 0 0 0-3-3.9", "M16 3.1a4 4 0 0 1 0 7.8"],
    x: ["M18 6 6 18", "m6 6 12 12"],
    copy: ["M9 9h11a1 1 0 0 1 1 1v11a1 1 0 0 1-1 1H9a1 1 0 0 1-1-1V10a1 1 0 0 1 1-1z", "M5 15H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v1"],
    check: ["M20 6 9 17l-5-5"],
    arrowUp: ["M12 19V5", "m5 12 7-7 7 7"],
    arrowDown: ["M12 5v14", "m19 12-7 7-7-7"],
    info: ["M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20z", "M12 16v-4", "M12 8h.01"],
    alert: ["M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20z", "M12 8v4", "M12 16h.01"],
  };
</script>

<script lang="ts">
  let { name, size = 18, stroke = 1.9 }: { name: IconName; size?: number; stroke?: number } = $props();
</script>

<svg
  class="icon"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width={stroke}
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  {#each PATHS[name] as d}<path {d} />{/each}
</svg>
