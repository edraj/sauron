import { mount } from 'svelte';
import '../src/app.css';
import DevicesInventory from '../src/pages/DevicesInventory.svelte';
import UsersExplorer from '../src/pages/UsersExplorer.svelte';
import SessionsList from '../src/pages/SessionsList.svelte';

// Seeded BEFORE the mount: `AppShell`'s `onMount` runs `sessionStore.load()`
// immediately, and setting these afterwards races the bootstrap chain into the
// "pick an app" redirect.
localStorage.setItem('sauron.org_id', 'org1');
localStorage.setItem('sauron.project_id', 'proj1');
localStorage.setItem('sauron.app_id', 'app1');

const PAGES = { devices: DevicesInventory, users: UsersExplorer, sessions: SessionsList } as const;
const which = (new URLSearchParams(location.search).get('page') ?? 'devices') as keyof typeof PAGES;

mount(PAGES[which] ?? DevicesInventory, { target: document.getElementById('app')! });
