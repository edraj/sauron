import { mount } from 'svelte';
// Bundled, not fetched: the dashboard ships offline (Docker, RPM, .deb), and
// `--font-sans` named Inter for years without ever loading it. Fontsource's
// CSS carries per-script `unicode-range`s, so the Arabic face only downloads
// when Arabic text is on screen.
import '@fontsource-variable/inter';
import '@fontsource-variable/noto-sans-arabic';
import './app.css';
import App from './App.svelte';

const target = document.getElementById('app');
if (!target) {
  throw new Error('Root element #app not found');
}

const app = mount(App, { target });

export default app;
