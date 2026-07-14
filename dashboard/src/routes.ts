import { wrap } from 'svelte-spa-router/wrap';
import type { Component } from 'svelte';
import { authStore } from './lib/stores/auth.svelte';

import Login from './pages/Login.svelte';
import Register from './pages/Register.svelte';
import Onboarding from './pages/Onboarding.svelte';
import Overview from './pages/Overview.svelte';
import Issues from './pages/Issues.svelte';
import IssueDetail from './pages/IssueDetail.svelte';
import Events from './pages/Events.svelte';
import Performance from './pages/Performance.svelte';
import SessionsList from './pages/SessionsList.svelte';
import SessionDetail from './pages/SessionDetail.svelte';
import UsersExplorer from './pages/UsersExplorer.svelte';
import PersonProfile from './pages/PersonProfile.svelte';
import DevicesInventory from './pages/DevicesInventory.svelte';
import DeviceDetail from './pages/DeviceDetail.svelte';
import ScreensList from './pages/ScreensList.svelte';
import ScreenDetail from './pages/ScreenDetail.svelte';
import FunnelBuilder from './pages/FunnelBuilder.svelte';
import JourneyExplorer from './pages/JourneyExplorer.svelte';
import Monitors from './pages/Monitors.svelte';
import Projects from './pages/Projects.svelte';
import Members from './pages/Members.svelte';
import SettingsApp from './pages/SettingsApp.svelte';
import Docs from './pages/Docs.svelte';
import Redirect from './lib/components/Redirect.svelte';

const authed = () => authStore.isAuthenticated;

// Svelte 5 components are functions; svelte-spa-router's `wrap` types against the
// legacy ComponentType, so we cast at the boundary.
function guarded(component: Component<never>) {
  return wrap({ component: component as never, conditions: [authed] });
}

export const routes = {
  '/login': Login,
  '/register': Register,
  '/onboarding': guarded(Onboarding as Component<never>),

  // Monitor
  '/overview': guarded(Overview as Component<never>),
  '/issues': guarded(Issues as Component<never>),
  '/issues/:id': guarded(IssueDetail as Component<never>),
  '/performance': guarded(Performance as Component<never>),

  // Explore
  '/events': guarded(Events as Component<never>),
  '/sessions': guarded(SessionsList as Component<never>),
  '/sessions/:id': guarded(SessionDetail as Component<never>),
  '/users': guarded(UsersExplorer as Component<never>),
  '/persons/:distinctId': guarded(PersonProfile as Component<never>),
  '/devices': guarded(DevicesInventory as Component<never>),
  '/devices/:key': guarded(DeviceDetail as Component<never>),
  '/screens': guarded(ScreensList as Component<never>),
  '/screens/:name': guarded(ScreenDetail as Component<never>),

  // Analyze
  '/funnels': guarded(FunnelBuilder as Component<never>),
  '/journeys': guarded(JourneyExplorer as Component<never>),

  // Uptime
  '/monitors': guarded(Monitors as Component<never>),

  // Settings
  '/projects': guarded(Projects as Component<never>),
  '/members': guarded(Members as Component<never>),
  '/settings': guarded(SettingsApp as Component<never>),

  // Docs / integration guides
  '/docs': guarded(Docs as Component<never>),

  '/': Redirect,
  '*': Redirect,
};
