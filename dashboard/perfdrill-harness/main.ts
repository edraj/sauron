import { mount } from 'svelte';
import '../src/app.css';
import Root from './Root.svelte';

// Seeded BEFORE the mount: AppShell's onMount runs sessionStore.load(), which
// reads these to resolve the scope. Set afterwards they race the bootstrap and
// leave every page on the "pick an app" redirect.
localStorage.setItem('sauron.org_id', 'org1');
localStorage.setItem('sauron.project_id', 'proj1');
localStorage.setItem('sauron.app_id', 'app1');

// The real router reads the hash at mount; land on Performance rather than the
// unmatched-route fallback so the first paint is the page under test.
if (!location.hash) location.hash = '#/performance';

mount(Root, { target: document.getElementById('app')! });
