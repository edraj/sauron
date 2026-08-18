import { mount } from 'svelte';
import '../src/app.css';
import SourceMaps from '../src/pages/SourceMaps.svelte';

localStorage.setItem('sauron.org_id', 'org1');
localStorage.setItem('sauron.project_id', 'proj1');
localStorage.setItem('sauron.app_id', 'app1');

mount(SourceMaps, { target: document.getElementById('app')! });
