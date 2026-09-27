import { mount } from 'svelte';
import '../src/app.css';
import Switcher from './Switcher.svelte';
import { sessionStore } from '../src/lib/stores/session.svelte';

// Seeded BEFORE the mount: AppShell's onMount runs sessionStore.load(), which
// reads these to resolve the scope. Set afterwards they race the bootstrap and
// leave the page on the "pick an app" redirect.
localStorage.setItem('sauron.org_id', 'org1');
localStorage.setItem('sauron.project_id', 'proj1');
localStorage.setItem('sauron.app_id', 'app1');

// `?session=` picks the initial fixture; the on-screen switcher swaps it in
// place, which is what reproduces the router reusing this page across sessions.
// The harness has to boot the session itself. It used to come free: every page
// wrapped itself in `AppShell`, whose `onMount` ran `sessionStore.load()`.
// Since the shell was hoisted into `App.svelte` (one shell for the whole app),
// a page mounted on its own never resolves `currentAppId`, every load effect
// returns early, and the page sits on its skeleton with an empty request log —
// which reads exactly like a broken page. Same fix as `listui-harness/main.ts`.
//
// AWAITED, because the real app waits too: `AppShell` renders a loading state
// until `sessionStore.loaded`, so a page never mounts against a half-resolved
// scope. Mounting first makes every page load twice — once before the
// environment resolves and once after — and the second set of requests then
// queues behind the first in the browser's per-URL cache lock, which doubles
// every delay this harness is trying to show.
await sessionStore.load();

mount(Switcher, { target: document.getElementById('app')! });
