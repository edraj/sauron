<script lang="ts">
  import type { Journey } from '../models';
  import { hueFromString } from '../utils/format';

  interface Props {
    journey: Journey;
    height?: number;
    // Max nodes kept per step (top-N by volume) to stay readable.
    topPerStep?: number;
  }

  let { journey, height = 460, topPerStep = 6 }: Props = $props();

  const NODE_W = 13;
  const PAD = 8;
  const WIDTH = 960;

  interface LaidNode {
    key: string;
    step: number;
    event: string;
    count: number;
    x: number;
    y: number;
    h: number;
    outAccum: number;
    inAccum: number;
    hue: number;
  }

  const layout = $derived.by(() => {
    const steps = journey.depth;
    // Top-N nodes per step.
    const byStep: Record<number, typeof journey.nodes> = {};
    for (const n of journey.nodes) {
      (byStep[n.step] ??= []).push(n);
    }
    const kept = new Map<string, LaidNode>();
    let maxTotal = 1;
    let maxNodes = 1;
    for (let s = 0; s < steps; s++) {
      const list = (byStep[s] ?? []).slice(0, topPerStep);
      const total = list.reduce((a, n) => a + n.count, 0);
      if (total > maxTotal) maxTotal = total;
      if (list.length > maxNodes) maxNodes = list.length;
    }
    const scale = (height - (maxNodes - 1) * PAD) / maxTotal;
    const colGap = steps > 1 ? (WIDTH - NODE_W) / (steps - 1) : 0;

    for (let s = 0; s < steps; s++) {
      const list = (byStep[s] ?? []).slice(0, topPerStep);
      const total = list.reduce((a, n) => a + n.count, 0);
      const stackH = total * scale + Math.max(0, list.length - 1) * PAD;
      let y = (height - stackH) / 2;
      for (const n of list) {
        const h = Math.max(2, n.count * scale);
        kept.set(`${s}|${n.event}`, {
          key: `${s}|${n.event}`,
          step: s,
          event: n.event,
          count: n.count,
          x: s * colGap,
          y,
          h,
          outAccum: 0,
          inAccum: 0,
          hue: hueFromString(n.event),
        });
        y += h + PAD;
      }
    }

    // Ribbons for links whose both endpoints survived the top-N cut.
    const ribbons: { path: string; hue: number; count: number }[] = [];
    const links = [...journey.links].sort((a, b) => a.from_step - b.from_step);
    for (const l of links) {
      const src = kept.get(`${l.from_step}|${l.from_event}`);
      const tgt = kept.get(`${l.from_step + 1}|${l.to_event}`);
      if (!src || !tgt) continue;
      const w = Math.max(1, l.count * scale);
      const sx = src.x + NODE_W;
      const sy0 = src.y + src.outAccum;
      const tx = tgt.x;
      const ty0 = tgt.y + tgt.inAccum;
      src.outAccum += w;
      tgt.inAccum += w;
      const mx = (sx + tx) / 2;
      const path =
        `M${sx},${sy0} C${mx},${sy0} ${mx},${ty0} ${tx},${ty0} ` +
        `L${tx},${ty0 + w} C${mx},${ty0 + w} ${mx},${sy0 + w} ${sx},${sy0 + w} Z`;
      ribbons.push({ path, hue: src.hue, count: l.count });
    }

    return { nodes: [...kept.values()], ribbons, steps };
  });
</script>

{#if journey.nodes.length === 0}
  <div class="sk-empty faint">Not enough event data to map journeys in this range.</div>
{:else}
  <div class="sk-wrap">
    <svg viewBox="0 0 {WIDTH} {height}" width="100%" height={height} preserveAspectRatio="xMidYMid meet">
      {#each layout.ribbons as r, i (i)}
        <path d={r.path} fill={`hsl(${r.hue} 60% 60% / 0.22)`} class="ribbon" />
      {/each}
      {#each layout.nodes as n (n.key)}
        <g class="node">
          <rect x={n.x} y={n.y} width={NODE_W} height={n.h} rx="3" fill={`hsl(${n.hue} 62% 60%)`}>
            <title>{n.event} · step {n.step + 1} · {n.count.toLocaleString()}</title>
          </rect>
          {#if n.h > 14}
            <text
              x={n.step === layout.steps - 1 ? n.x - 6 : n.x + NODE_W + 6}
              y={n.y + n.h / 2}
              text-anchor={n.step === layout.steps - 1 ? 'end' : 'start'}
              dominant-baseline="middle"
              class="node-label"
            >{n.event}</text>
          {/if}
        </g>
      {/each}
    </svg>
  </div>
{/if}

<style>
  .sk-wrap {
    width: 100%;
    overflow-x: auto;
  }
  svg {
    display: block;
    min-width: 640px;
  }
  .ribbon {
    transition: fill 0.12s ease;
  }
  .ribbon:hover {
    fill-opacity: 1;
  }
  .node-label {
    font-family: var(--font-mono);
    font-size: 10.5px;
    fill: var(--text-muted);
  }
  .sk-empty {
    padding: 40px;
    text-align: center;
    font-size: 13px;
  }
</style>
