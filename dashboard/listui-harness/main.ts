import { mount } from 'svelte';
import '../src/app.css';
import Switcher from './Switcher.svelte';
import { sessionStore } from '../src/lib/stores/session.svelte';

// Seeded BEFORE the load: `sessionStore.load()` reads these to resolve the
// scope. Set afterwards they race the bootstrap and leave every page on the
// "pick an app" redirect.
localStorage.setItem('sauron.org_id', 'org1');
localStorage.setItem('sauron.project_id', 'proj1');
localStorage.setItem('sauron.app_id', 'app1');

// The harness has to boot the session itself. It used to come free: every page
// wrapped itself in `AppShell`, whose `onMount` ran `sessionStore.load()`.
// Since the shell was hoisted into `App.svelte` (one shell for the whole app),
// a page mounted on its own never resolves `currentAppId`, every predicate
// effect returns early, and the page sits on its skeleton with an empty request
// log — which reads exactly like a broken page.
void sessionStore.load();

mount(Switcher, { target: document.getElementById('app')! });
