import { mount } from 'svelte';
import '../src/app.css';
import PersonProfile from '../src/pages/PersonProfile.svelte';

/**
 * Seeded BEFORE the mount, because `AppShell`'s `onMount` immediately runs
 * `sessionStore.load()`, which reads these keys to resolve the current scope.
 * Setting them afterwards would race the bootstrap chain and leave the page on
 * the "pick an app" redirect instead of the profile.
 */
localStorage.setItem('sauron.org_id', 'org1');
localStorage.setItem('sauron.project_id', 'proj1');
localStorage.setItem('sauron.app_id', 'app1');

// `?person=` switches fixtures — anything starting with `quiet` returns the
// no-activity profile, which is how the empty state gets checked.
const distinctId = new URLSearchParams(location.search).get('person') ?? 'ana@example.com';

// `?theme=` is handled by an inline script in index.html, not here: `themeStore`
// reads localStorage in a module-scope constructor, which import hoisting runs
// before this file's body. See the comment beside that script.

mount(PersonProfile, {
  target: document.getElementById('app')!,
  props: { params: { distinctId: encodeURIComponent(distinctId) } },
});
