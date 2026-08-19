import { mount } from 'svelte';
import '../src/app.css';
import Root from './Root.svelte';

// Seeded BEFORE the mount: AppShell's onMount runs sessionStore.load(), which
// reads these to resolve the scope. Set afterwards they race the bootstrap and
// leave every page on the "pick an app" redirect.
localStorage.setItem('sauron.org_id', 'org1');
localStorage.setItem('sauron.project_id', 'proj1');
localStorage.setItem('sauron.app_id', 'app1');

if (!location.hash) location.hash = '#/account';

mount(Root, { target: document.getElementById('app')! });
