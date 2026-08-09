<script lang="ts">
  /**
   * A placeholder occupying roughly the space its content will.
   *
   * Distinct from `Spinner`, and the distinction is the point. A spinner says
   * "something is happening somewhere"; a skeleton says "this specific block is
   * arriving, and it will be about this big". On a page whose sections load
   * independently that matters — a page of four spinners reads as four separate
   * problems, while a page of four skeletons reads as one page filling in.
   *
   * Sized in `rows` rather than pixels so a caller describes the shape of the
   * content instead of guessing at its height.
   */
  interface Props {
    /** How many placeholder lines to draw. */
    rows?: number;
    /** Height of each line. Use a larger value for chart/tile blocks. */
    height?: string;
    /** Accessible description of what is loading. */
    label?: string;
  }
  let { rows = 3, height = '14px', label = 'Loading' }: Props = $props();
</script>

<!--
  `aria-busy` + a polite live region rather than `role="progressbar"`: a screen
  reader needs to know the region is filling in, not to track a percentage we do
  not have.
-->
<div class="sk" aria-busy="true" aria-live="polite" aria-label={label}>
  {#each Array(rows) as _, i (i)}
    <div
      class="sk-row"
      style="height: {height}; width: {i === rows - 1 && rows > 1 ? '60%' : '100%'}"
    ></div>
  {/each}
</div>

<style>
  .sk {
    display: flex;
    flex-direction: column;
    gap: 10px;
    width: 100%;
  }
  .sk-row {
    border-radius: 4px;
    /* Two stops of the same token rather than a hard-coded grey, so the
       placeholder tracks the theme instead of going invisible in one of them. */
    background: linear-gradient(
      90deg,
      var(--border, #2a2a2a) 25%,
      var(--surface-2, #333) 50%,
      var(--border, #2a2a2a) 75%
    );
    background-size: 200% 100%;
    animation: sk-shimmer 1.4s ease-in-out infinite;
  }
  @keyframes sk-shimmer {
    0% {
      background-position: 200% 0;
    }
    100% {
      background-position: -200% 0;
    }
  }
  /* A shimmer is decoration, and for some vestibular conditions it is worse than
     decoration. The placeholder still conveys "loading" through its shape and
     `aria-busy`, so removing the motion costs nothing. */
  @media (prefers-reduced-motion: reduce) {
    .sk-row {
      animation: none;
    }
  }
</style>
