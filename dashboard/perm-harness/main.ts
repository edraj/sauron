import { mount } from 'svelte';
import '../src/app.css';
import Harness from './Harness.svelte';

localStorage.setItem('sauron.org_id', 'org1');
localStorage.setItem('sauron.project_id', 'proj1');
localStorage.setItem('sauron.app_id', 'app1');

mount(Harness, { target: document.getElementById('app')! });
