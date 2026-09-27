import { mount } from 'svelte';
import '../src/app.css';
import Root from './Root.svelte';
import { sessionStore } from '../src/lib/stores/session.svelte';

// Seeded BEFORE the load: `sessionStore.load()` reads these to resolve the
// scope. Set afterwards they race the bootstrap and leave every page on the
// "pick an app" redirect.
localStorage.setItem('sauron.org_id', 'org1');
localStorage.setItem('sauron.project_id', 'proj1');
localStorage.setItem('sauron.app_id', 'app1');

// The harness boots the session itself — see `listui-harness/main.ts` for why
// a page mounted on its own otherwise sits on its skeleton with an empty
// request log.
//
// AWAITED, because the real app waits too: `AppShell` renders a loading state
// until `sessionStore.loaded`, so a page never mounts against a half-resolved
// scope. Mounting first makes every page load twice — once before the
// environment resolves and once after — and the second set of requests then
// queues behind the first in the browser's per-URL cache lock, which doubles
// every delay this harness is trying to show.
await sessionStore.load();

mount(Root, { target: document.getElementById('app')! });
