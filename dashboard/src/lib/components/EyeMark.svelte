<script lang="ts">
  // The flame-colored Sauron eye, drawn without a background so it sits beside
  // the name on either theme. The favicon is this same eye on a dark square.
  // `size` is the rendered width in px; the height follows the eye's shape.
  let { size = 24 }: { size?: number } = $props();
</script>

<svg
  class="eye-mark"
  width={size}
  height={(size * 14.25) / 28}
  viewBox="2 8.875 28 14.25"
  role="img"
  aria-label="Sauron"
>
  <defs>
    <radialGradient id="em-fire" cx="50%" cy="52%" r="62%">
      <stop offset="0%" stop-color="#fff5cc" />
      <stop offset="22%" stop-color="#ffd451" />
      <stop offset="50%" stop-color="#ff8f22" />
      <stop offset="80%" stop-color="#f4520c" />
      <stop offset="100%" stop-color="#c42a05" />
    </radialGradient>
  </defs>
  <path d="M2 16C8 6.5 24 6.5 30 16 24 25.5 8 25.5 2 16Z" fill="url(#em-fire)" />
  <path
    d="M15.61 8.88Q16 8.87 16.39 8.88C18.77 12.17 18.77 19.83 16.39 23.12Q16 23.13 15.61 23.12C13.23 19.83 13.23 12.17 15.61 8.88Z"
    fill="#0c0a09"
  />
</svg>

<style>
  .eye-mark {
    display: block;
    flex-shrink: 0;
  }
</style>
