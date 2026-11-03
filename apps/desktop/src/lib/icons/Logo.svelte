<script lang="ts">
  let { size = 28 }: { size?: number } = $props();
  // Unique per instance so several logos on one page don't share a clipPath id.
  const clipId = `peerly-logo-${Math.random().toString(36).slice(2, 8)}`;
</script>

<!-- Peerly mark: two overlapping circles (two peers meeting). Matches app-icon.svg. -->
<svg width={size} height={size} viewBox="0 0 32 32" fill="none" aria-hidden="true">
  <defs>
    <clipPath id={clipId}><circle cx="19.5" cy="16" r="8.5" /></clipPath>
  </defs>
  <circle cx="12.5" cy="16" r="8.5" fill="#f97316" clip-path="url(#{clipId})" />
  <circle cx="12.5" cy="16" r="8.5" stroke="#f97316" stroke-width="3" />
  <circle cx="19.5" cy="16" r="8.5" stroke="#f4f4f5" stroke-width="3" />
</svg>
