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
  import Bell from '@lucide/svelte/icons/bell';
  import BookOpen from '@lucide/svelte/icons/book-open';
  import Braces from '@lucide/svelte/icons/braces';
  import Calendar from '@lucide/svelte/icons/calendar';
  import ChartColumn from '@lucide/svelte/icons/chart-column';
  import Check from '@lucide/svelte/icons/check';
  import ChevronDown from '@lucide/svelte/icons/chevron-down';
  import ChevronLeft from '@lucide/svelte/icons/chevron-left';
  import ChevronRight from '@lucide/svelte/icons/chevron-right';
  import ChevronUp from '@lucide/svelte/icons/chevron-up';
  import CircleCheck from '@lucide/svelte/icons/circle-check';
  import CircleX from '@lucide/svelte/icons/circle-x';
  import Clock from '@lucide/svelte/icons/clock';
  import Compass from '@lucide/svelte/icons/compass';
  import Copy from '@lucide/svelte/icons/copy';
  import Diamond from '@lucide/svelte/icons/diamond';
  import Download from '@lucide/svelte/icons/download';
  import EyeOff from '@lucide/svelte/icons/eye-off';
  import Folders from '@lucide/svelte/icons/folders';
  import Funnel from '@lucide/svelte/icons/funnel';
  import Globe from '@lucide/svelte/icons/globe';
  import Hash from '@lucide/svelte/icons/hash';
  import Inbox from '@lucide/svelte/icons/inbox';
  import Info from '@lucide/svelte/icons/info';
  import KeyRound from '@lucide/svelte/icons/key-round';
  import Layers from '@lucide/svelte/icons/layers';
  import LayoutDashboard from '@lucide/svelte/icons/layout-dashboard';
  import LayoutPanelTop from '@lucide/svelte/icons/layout-panel-top';
  import LifeBuoy from '@lucide/svelte/icons/life-buoy';
  import Lock from '@lucide/svelte/icons/lock';
  import Monitor from '@lucide/svelte/icons/monitor';
  import MonitorSmartphone from '@lucide/svelte/icons/monitor-smartphone';
  import Moon from '@lucide/svelte/icons/moon';
  import Package from '@lucide/svelte/icons/package';
  import RefreshCw from '@lucide/svelte/icons/refresh-cw';
  import ScrollText from '@lucide/svelte/icons/scroll-text';
  import Search from '@lucide/svelte/icons/search';
  import Server from '@lucide/svelte/icons/server';
  import Settings from '@lucide/svelte/icons/settings';
  import ShieldAlert from '@lucide/svelte/icons/shield-alert';
  import ShieldCheck from '@lucide/svelte/icons/shield-check';
  import Smartphone from '@lucide/svelte/icons/smartphone';
  import Sun from '@lucide/svelte/icons/sun';
  import Terminal from '@lucide/svelte/icons/terminal';
  import Timer from '@lucide/svelte/icons/timer';
  import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
  import User from '@lucide/svelte/icons/user';
  import Users from '@lucide/svelte/icons/users';
  import Repeat from '@lucide/svelte/icons/repeat';
  import Waypoints from '@lucide/svelte/icons/waypoints';
  import Workflow from '@lucide/svelte/icons/workflow';
  import X from '@lucide/svelte/icons/x';
  import Zap from '@lucide/svelte/icons/zap';

  /** Semantic name → Lucide component. The single source of truth for icons. */
  export const iconRegistry = {
    'arrow-left': ArrowLeft,
    'arrow-right': ArrowRight,
    'arrow-up-right': ArrowUpRight,
    bell: Bell,
    'book-open': BookOpen,
    braces: Braces,
    calendar: Calendar,
    'chart-column': ChartColumn,
    check: Check,
    'chevron-down': ChevronDown,
    'chevron-left': ChevronLeft,
    'chevron-right': ChevronRight,
    'chevron-up': ChevronUp,
    'circle-check': CircleCheck,
    'circle-x': CircleX,
    clock: Clock,
    compass: Compass,
    copy: Copy,
    diamond: Diamond,
    download: Download,
    'eye-off': EyeOff,
    folders: Folders,
    funnel: Funnel,
    repeat: Repeat,
    globe: Globe,
    hash: Hash,
    inbox: Inbox,
    info: Info,
    'key-round': KeyRound,
    layers: Layers,
    'layout-dashboard': LayoutDashboard,
    'layout-panel-top': LayoutPanelTop,
    'life-buoy': LifeBuoy,
    lock: Lock,
    monitor: Monitor,
    'monitor-smartphone': MonitorSmartphone,
    moon: Moon,
    package: Package,
    refresh: RefreshCw,
    'scroll-text': ScrollText,
    search: Search,
    server: Server,
    settings: Settings,
    'shield-alert': ShieldAlert,
    'shield-check': ShieldCheck,
    smartphone: Smartphone,
    sun: Sun,
    terminal: Terminal,
    timer: Timer,
    'triangle-alert': TriangleAlert,
    user: User,
    users: Users,
    waypoints: Waypoints,
    workflow: Workflow,
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

<!--
  `data-icon` carries the semantic name onto the rendered `<svg>`. Lucide
  spreads unknown props straight through, and the attribute is what lets
  `app.css` mirror the directional glyphs (chevrons, arrows) under `dir="rtl"`
  from one rule, instead of every call site knowing about direction. Lucide's
  own `lucide-*` class could serve the same purpose, but it is an internal
  naming detail of the package; this uses the registry's vocabulary.
-->
{#if Glyph}
  <Glyph {size} {strokeWidth} class={klass} data-icon={name} aria-hidden="true" />
{/if}
