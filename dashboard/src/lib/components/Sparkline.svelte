<script lang="ts">
  interface Props {
    data: number[];
    width?: number;
    height?: number;
    color?: string;
    fill?: boolean;
    strokeWidth?: number;
  }

  let {
    data,
    width = 120,
    height = 32,
    color = 'var(--primary)',
    fill = true,
    strokeWidth = 1.5,
  }: Props = $props();

  const pad = 2;

  const geom = $derived.by(() => {
    const n = data.length;
    if (n === 0) return { line: '', area: '' };
    const max = Math.max(...data, 1);
    const min = Math.min(...data, 0);
    const span = max - min || 1;
    const stepX = n > 1 ? (width - pad * 2) / (n - 1) : 0;
    const pts = data.map((v, i) => {
      const x = pad + i * stepX;
      const y = height - pad - ((v - min) / span) * (height - pad * 2);
      return [x, y] as const;
    });
    const line = pts.map(([x, y], i) => `${i === 0 ? 'M' : 'L'}${x.toFixed(1)},${y.toFixed(1)}`).join(' ');
    const last = pts[pts.length - 1][0];
    const area = `${line} L${last.toFixed(1)},${height} L${pad},${height} Z`;
    return { line, area };
  });
</script>

<svg
  class="spark"
  viewBox="0 0 {width} {height}"
  width={width}
  height={height}
  preserveAspectRatio="none"
  role="img"
  aria-hidden="true"
  style="--spark:{color}"
>
  {#if data.length > 0}
    {#if fill}
      <path class="spark-area" d={geom.area} />
    {/if}
    <path class="spark-line" d={geom.line} fill="none" stroke-width={strokeWidth} />
  {/if}
</svg>

<style>
  .spark {
    display: block;
    max-width: 100%;
    overflow: visible;
  }
  .spark-line {
    stroke: var(--spark);
    stroke-linejoin: round;
    stroke-linecap: round;
    vector-effect: non-scaling-stroke;
  }
  .spark-area {
    fill: color-mix(in srgb, var(--spark) 16%, transparent);
    stroke: none;
  }
</style>
