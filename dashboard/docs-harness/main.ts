import { mount } from 'svelte';
import '../src/app.css';
import Docs from '../src/pages/Docs.svelte';

/**
 * Seeded BEFORE the mount: `AppShell`'s `onMount` runs `sessionStore.load()`
 * immediately, and it reads these to resolve the current scope. Set them after
 * and the page lands on the "pick an app" redirect instead of the docs.
 */
localStorage.setItem('sauron.org_id', 'org1');
localStorage.setItem('sauron.project_id', 'proj1');
localStorage.setItem('sauron.app_id', 'app1');

mount(Docs, { target: document.getElementById('app')! });
