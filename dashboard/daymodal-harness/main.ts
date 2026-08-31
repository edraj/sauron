import { mount } from 'svelte';
import '../src/app.css';
import Root from './Root.svelte';
import { sessionStore } from '../src/lib/stores/session.svelte';

// Seeded BEFORE the mount: the session bootstrap reads these to resolve scope.
localStorage.setItem('sauron.org_id', 'org1');
localStorage.setItem('sauron.project_id', 'proj1');
localStorage.setItem('sauron.app_id', 'app1');

if (!location.hash) location.hash = '#/performance';

// AppShell used to do this on mount, but the shell was hoisted into
// App.svelte — which this harness deliberately does not mount. Without it
// `currentAppId` stays null, the page's effect never fires, and Performance
// renders its chrome with no charts at all: a harness that silently tests
// nothing. Boot the store here instead.
await sessionStore.load();

mount(Root, { target: document.getElementById('app')! });
