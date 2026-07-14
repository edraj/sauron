<!--
  Central icon component. All dashboard icons are Lucide, addressed by a
  semantic kebab-case name so callers stay declarative (`<Icon name="search" />`)
  and the icon set lives in exactly one place — the `iconRegistry` below.

  Icons inherit `currentColor` for their stroke, so they take the surrounding
  text color (matching how the previous emoji glyphs behaved inside colored
  spans). Size is in px; stroke width defaults to Lucide's 2.
-->
<script module lang="ts">
  import type { Component } from 'svelte';
  import ArrowLeft from '@lucide/svelte/icons/arrow-left';
  import ArrowRight from '@lucide/svelte/icons/arrow-right';
  import ArrowUpRight from '@lucide/svelte/icons/arrow-up-right';
  import BookOpen from '@lucide/svelte/icons/book-open';
  import Braces from '@lucide/svelte/icons/braces';
  import ChartColumn from '@lucide/svelte/icons/chart-column';
  import Check from '@lucide/svelte/icons/check';
  import ChevronDown from '@lucide/svelte/icons/chevron-down';
  import ChevronLeft from '@lucide/svelte/icons/chevron-left';
  import ChevronRight from '@lucide/svelte/icons/chevron-right';
  import CircleCheck from '@lucide/svelte/icons/circle-check';
  import CircleX from '@lucide/svelte/icons/circle-x';
  import Clock from '@lucide/svelte/icons/clock';
  import Compass from '@lucide/svelte/icons/compass';
  import Copy from '@lucide/svelte/icons/copy';
  import Diamond from '@lucide/svelte/icons/diamond';
  import Folders from '@lucide/svelte/icons/folders';
  import Funnel from '@lucide/svelte/icons/funnel';
  import Globe from '@lucide/svelte/icons/globe';
  import Hash from '@lucide/svelte/icons/hash';
  import Inbox from '@lucide/svelte/icons/inbox';
  import Info from '@lucide/svelte/icons/info';
  import KeyRound from '@lucide/svelte/icons/key-round';
  import LayoutDashboard from '@lucide/svelte/icons/layout-dashboard';
  import LayoutPanelTop from '@lucide/svelte/icons/layout-panel-top';
  import LifeBuoy from '@lucide/svelte/icons/life-buoy';
  import Lock from '@lucide/svelte/icons/lock';
  import Monitor from '@lucide/svelte/icons/monitor';
  import MonitorSmartphone from '@lucide/svelte/icons/monitor-smartphone';
  import Moon from '@lucide/svelte/icons/moon';
  import Package from '@lucide/svelte/icons/package';
  import Search from '@lucide/svelte/icons/search';
  import Server from '@lucide/svelte/icons/server';
  import Settings from '@lucide/svelte/icons/settings';
  import Smartphone from '@lucide/svelte/icons/smartphone';
  import Sun from '@lucide/svelte/icons/sun';
  import Terminal from '@lucide/svelte/icons/terminal';
  import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
  import User from '@lucide/svelte/icons/user';
  import Users from '@lucide/svelte/icons/users';
  import Waypoints from '@lucide/svelte/icons/waypoints';
  import X from '@lucide/svelte/icons/x';
  import Zap from '@lucide/svelte/icons/zap';

  /** Semantic name → Lucide component. The single source of truth for icons. */
  export const iconRegistry = {
    'arrow-left': ArrowLeft,
    'arrow-right': ArrowRight,
    'arrow-up-right': ArrowUpRight,
    'book-open': BookOpen,
    braces: Braces,
    'chart-column': ChartColumn,
    check: Check,
    'chevron-down': ChevronDown,
    'chevron-left': ChevronLeft,
    'chevron-right': ChevronRight,
    'circle-check': CircleCheck,
    'circle-x': CircleX,
    clock: Clock,
    compass: Compass,
    copy: Copy,
    diamond: Diamond,
    folders: Folders,
    funnel: Funnel,
    globe: Globe,
    hash: Hash,
    inbox: Inbox,
    info: Info,
    'key-round': KeyRound,
    'layout-dashboard': LayoutDashboard,
    'layout-panel-top': LayoutPanelTop,
    'life-buoy': LifeBuoy,
    lock: Lock,
    monitor: Monitor,
    'monitor-smartphone': MonitorSmartphone,
    moon: Moon,
    package: Package,
    search: Search,
    server: Server,
    settings: Settings,
    smartphone: Smartphone,
    sun: Sun,
    terminal: Terminal,
    'triangle-alert': TriangleAlert,
    user: User,
    users: Users,
    waypoints: Waypoints,
    x: X,
    zap: Zap,
  } satisfies Record<string, Component>;

  /** A valid icon name accepted by {@link Icon}. */
  export type IconName = keyof typeof iconRegistry;
</script>

<script lang="ts">
  interface Props {
    /** Semantic icon name; see {@link iconRegistry}. */
    name: IconName;
    /** Pixel size (width & height). */
    size?: number;
    /** Stroke width; Lucide default is 2. */
    strokeWidth?: number;
    /** Extra classes forwarded to the underlying `<svg>`. */
    class?: string;
  }

  let { name, size = 16, strokeWidth = 2, class: klass = '' }: Props = $props();

  const Glyph = $derived(iconRegistry[name]);
</script>

{#if Glyph}
  <Glyph {size} {strokeWidth} class={klass} aria-hidden="true" />
{/if}
