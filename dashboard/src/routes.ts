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
import WorkflowsList from './pages/WorkflowsList.svelte';
import ActiveUsers from './pages/ActiveUsers.svelte';
import FunnelBuilder from './pages/FunnelBuilder.svelte';
import JourneyExplorer from './pages/JourneyExplorer.svelte';
import Monitors from './pages/Monitors.svelte';
import Alerts from './pages/Alerts.svelte';
import MonitorDetail from './pages/MonitorDetail.svelte';
import Storage from './pages/Storage.svelte';
import Inspector from './pages/Inspector.svelte';
import SourceMaps from './pages/SourceMaps.svelte';
import Projects from './pages/Projects.svelte';
import Account from './pages/Account.svelte';
import Members from './pages/Members.svelte';
import SettingsApp from './pages/SettingsApp.svelte';
import Docs from './pages/Docs.svelte';
import ChangePassword from './pages/ChangePassword.svelte';
import ForgotPassword from './pages/ForgotPassword.svelte';
import ResetPassword from './pages/ResetPassword.svelte';
import Unsubscribe from './pages/Unsubscribe.svelte';
import Redirect from './lib/components/Redirect.svelte';

const authed = () => authStore.isAuthenticated;

// A pending password change blocks every authenticated page except the one
// that can resolve it (the server enforces this for real via Task 7's
// extractor gate — this is convenience only). Deliberately does NOT navigate
// itself: App.svelte's `conditionsFailed` handler does that, so there is a
// single place deciding between /login and /change-password instead of two
// navigations racing each other.
function passwordCurrent(): boolean {
  return !authStore.mustChangePassword;
}

// Svelte 5 components are functions; svelte-spa-router's `wrap` types against the
// legacy ComponentType, so we cast at the boundary.
function guarded(component: Component<never>) {
  return wrap({ component: component as never, conditions: [authed, passwordCurrent] });
}

export const routes = {
  '/login': Login,
  '/register': Register,
  // Both are BARE — no wrap, no guarded(), no `authed` or `passwordCurrent`
  // condition. Wrapping either would fire conditionsFailed, which pushes to
  // /login or /change-password and makes a reset link unusable.
  '/forgot-password': ForgotPassword,
  '/reset-password': ResetPassword,
  // Ungated on passwordCurrent — otherwise a temp-password holder redirected
  // here would immediately redirect right back to itself.
  '/change-password': wrap({ component: ChangePassword as never, conditions: [authed] }),
  // `conditions: []` — not `guarded()`, and deliberately NOT in App.svelte's
  // PUBLIC_ROUTES either. That array drives an $effect that pushes
  // authenticated users OFF those paths, which is exactly wrong here: a
  // logged-in user clicking an unsubscribe link must still see the
  // confirmation.
  '/unsubscribe': wrap({ component: Unsubscribe as never, conditions: [] }),
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
  '/workflows': guarded(WorkflowsList as Component<never>),

  // Analyze
  '/active-users': guarded(ActiveUsers as Component<never>),
  '/funnels': guarded(FunnelBuilder as Component<never>),
  '/journeys': guarded(JourneyExplorer as Component<never>),

  // Uptime
  '/monitors': guarded(Monitors as Component<never>),
  '/monitors/:id': guarded(MonitorDetail as Component<never>),

  // Alerting
  '/alerts': guarded(Alerts as Component<never>),

  // Settings
  '/projects': guarded(Projects as Component<never>),
  '/account': guarded(Account as Component<never>),
  '/members': guarded(Members as Component<never>),
  '/settings': guarded(SettingsApp as Component<never>),
  '/source-maps': guarded(SourceMaps as Component<never>),

  // Admin. Route conditions deliberately do NOT carry permissions: a failed
  // condition fires `conditionsFailed`, which navigates, and a deep link the
  // user cannot open should keep its URL and explain itself. Instead AppShell
  // resolves the current path through PAGE_ACCESS (lib/models/page-access.ts)
  // and renders PermissionDenied in place of the page. Both layers stay
  // cosmetic either way — every endpoint 403s on its own.
  '/storage': guarded(Storage as Component<never>),
  '/inspector': guarded(Inspector as Component<never>),

  // Docs / integration guides
  '/docs': guarded(Docs as Component<never>),

  '/': Redirect,
  '*': Redirect,
};
