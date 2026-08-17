import { mount } from 'svelte';
import '../src/app.css';
// Importing the real store is what stamps `data-theme` on <html>; app.css is
// dark-first, so without it the light fixtures would render dark tokens.
import '../src/lib/stores/theme.svelte';
import Harness from './Harness.svelte';

mount(Harness, { target: document.getElementById('app')! });
