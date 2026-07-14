<script lang="ts">
  // The flame-colored Sauron eye mark. Matches the favicon.
  // `size` is the rendered square in px.
  let { size = 28 }: { size?: number } = $props();
</script>

<svg
  class="eye-mark"
  width={size}
  height={size}
  viewBox="0 0 32 32"
  role="img"
  aria-label="Sauron"
>
  <defs>
    <radialGradient id="em-fire" cx="50%" cy="52%" r="62%">
      <stop offset="0%" stop-color="#fff7d6" />
      <stop offset="20%" stop-color="#ffd451" />
      <stop offset="48%" stop-color="#ff8f22" />
      <stop offset="76%" stop-color="#f14e0b" />
      <stop offset="100%" stop-color="#a81a03" />
    </radialGradient>
  </defs>
  <rect width="32" height="32" rx="7" fill="#0c0a09" />
  <path d="M2 16C8 6.5 24 6.5 30 16 24 25.5 8 25.5 2 16Z" fill="url(#em-fire)" />
  <path d="M16 7.2c2.4 3.8 2.4 13.8 0 17.6-2.4-3.8-2.4-13.8 0-17.6Z" fill="#0c0a09" />
</svg>

<style>
  .eye-mark {
    display: block;
    border-radius: 7px;
  }
</style>
