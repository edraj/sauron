import { mount } from 'svelte';
import '../src/app.css';
import Switcher from './Switcher.svelte';

// Seeded BEFORE the mount: AppShell's onMount runs sessionStore.load(), which
// reads these to resolve the scope. Set afterwards they race the bootstrap and
// leave the page on the "pick an app" redirect.
localStorage.setItem('sauron.org_id', 'org1');
localStorage.setItem('sauron.project_id', 'proj1');
localStorage.setItem('sauron.app_id', 'app1');

// `?session=` picks the initial fixture; the on-screen switcher swaps it in
// place, which is what reproduces the router reusing this page across sessions.
mount(Switcher, { target: document.getElementById('app')! });
