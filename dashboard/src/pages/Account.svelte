<script lang="ts">
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import ClientPager from '../lib/components/ClientPager.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import ConfirmDialog from '../lib/components/ui/ConfirmDialog.svelte';
  import NotificationSubscriptions from '../lib/components/account/NotificationSubscriptions.svelte';
  import LanguagePicker from '../lib/components/account/LanguagePicker.svelte';
  import { authStore } from '../lib/stores/auth.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewCache, viewKey } from '../lib/stores/view-cache';
  import { errorMessage } from '../lib/api/client';
  import { t } from '../lib/i18n';
  import { listMySessions, revokeMyOtherSessions, revokeMySession } from '../lib/api/account';
  import {
    allSameIp,
    describeSession,
    hasCurrentSession,
    otherSessionCount,
  } from '../lib/models/account-sessions';
  import { setOffsetPage, setOffsetSort, type OffsetListState } from '../lib/models/list-state';
  import { SESSION_DEFAULT_SORT, sessionAccessor } from '../lib/models/account-session-sort';
  import { pageSlice } from '../lib/models/paginate';
  import { sortRows } from '../lib/models/sort-rows';
  import type { SortDir } from '../lib/models/sort';
  import type { AccountSession } from '../lib/models';

  /** Rows per page. The list arrives whole, so this is a rendering budget only. */
  const PAGE = 25;

  // Cached view (lib/stores/cached-view.svelte.ts): the cached device list paints
  // instantly on return, then refreshes behind the Refresh button's spinner.
  // Re-exposed under the template's existing names, so the markup is unchanged
  // apart from that button.
  const sessionsView = new CachedView<AccountSession[]>();

  const sessions = $derived(sessionsView.data ?? []);
  const loading = $derived(sessionsView.loading);
  const revalidating = $derived(sessionsView.revalidating);
  const error = $derived(sessionsView.error);

  let showRevoked = $state(false);
  let busy = $state(false);

  // One dialog for both verbs, matching Members.svelte's requestToggle /
  // confirmDeactivate shape. `$state.raw` because this is replaced wholesale and
  // nothing reads through a proxy.
  let pending = $state.raw<{ kind: 'one'; id: string; label: string } | { kind: 'all' } | null>(
    null,
  );

  // `/v1/me/sessions` returns every session in one response, so the sort and
  // the pager both run here, over the SAME array: order the whole list first,
  // then take a window out of it. Sorting the window instead would reorder only
  // what is on screen while presenting itself as having ordered everything.
  //
  // Sort and offset are one `OffsetListState` rather than two variables because
  // `setOffsetSort` resets the offset as part of applying a sort.
  let list = $state<OffsetListState>({ sort: SESSION_DEFAULT_SORT, offset: 0 });

  // `sortRows` copies before sorting, which is load-bearing rather than tidy:
  // `sessionsView.data` is the VERY ARRAY the view cache holds, handed back by
  // reference (`cached-view.svelte.ts` says so, and `$state.raw` keeps that
  // identity exact), so an in-place sort would reorder the cached payload for
  // every later reader.
  //
  // This replaces the page's own `sortSessions(sessions)` call. Two orderings
  // applied in sequence is a bug waiting for someone to change one of them, and
  // the seed (`last_used desc`) is what `sortSessions` produced apart from its
  // current-session-first rule — the current device is still marked by its
  // "This device" badge.
  const sorted = $derived(sortRows(sessions, sessionAccessor(list.sort.key), list.sort.dir));
  const live = $derived(sorted.filter((s) => s.revoked_at === null));
  const revoked = $derived(sorted.filter((s) => s.revoked_at !== null));

  /**
   * The rows the table actually renders: live first, then revoked, each group
   * in the chosen order.
   *
   * The grouping is a FIXED primary key and is stated here rather than hidden
   * in a comparator, because a header click orders within it and not across it.
   * It stays because the two groups render different content in the same
   * columns — a live row's last column offers "Sign out" while a revoked one
   * explains why it ended, and the Last-used cell shows a use time for one and
   * a sign-out time for the other. Interleaving them would put two meanings in
   * one column and order it by neither.
   *
   * With "Show recent sign-outs" off — the default — the endpoint returns no
   * revoked rows at all, so `visible` is exactly `live` and every header orders
   * the entire table.
   */
  const visible = $derived([...live, ...revoked]);
  // `page.rows` is the window; `visible` stays the thing the pager measures.
  const page = $derived(pageSlice(visible, list.offset, PAGE));

  function onsort(key: string, columnDefault: SortDir) {
    list = setOffsetSort(list, key, columnDefault);
  }

  const otherCount = $derived(otherSessionCount(sessions));
  const hasCurrent = $derived(hasCurrentSession(sessions));
  const proxied = $derived(allSameIp(sessions));

  /** Every key this page writes, for prefix invalidation after a revoke. */
  const SESSIONS_VIEW = 'account.sessions';

  /**
   * `force` bypasses the fresh-window short-circuit: an explicit Refresh click
   * means "go to the network now", and honouring the cache there makes the
   * control look broken.
   *
   * `scopeKey` is in the key unconditionally, as it is on every other cached
   * view. It carries the selected environment, which the axios interceptor adds
   * to requests without it appearing in any caller argument, so leaving it out
   * is how one scope's rows get served as another's. Keep it here even though
   * `/v1/me/*` happens not to be environment-scoped today — the cost is one
   * refetch when the selection changes, and the failure mode it prevents is a
   * data leak rather than mere staleness.
   */
  async function load(force = false) {
    await sessionsView.load(
      viewKey(SESSIONS_VIEW, sessionStore.scopeKey, showRevoked),
      () => listMySessions(showRevoked),
      force,
    );
  }

  // Not forced: flipping the toggle should paint a cached list for the new key
  // instantly. The effect below re-runs on `showRevoked` too, but both calls
  // resolve to the same cache key and `viewCache.dedupe` collapses them into one
  // request — where this previously issued two.
  async function toggleHistory() {
    showRevoked = !showRevoked;
    // A FILTER CHANGE RESETS THE OFFSET, exactly as a sort change does through
    // `setOffsetSort`. This toggle changes which rows the endpoint returns, so
    // without the reset, hiding sign-outs while on page 3 of a list that is
    // suddenly one page long leaves you looking at an empty table with Prev as
    // the only way out.
    list = setOffsetPage(list, 0);
    await load();
  }

  function requestRevokeOne(s: AccountSession) {
    pending = { kind: 'one', id: s.id, label: describeSession(s) };
  }

  function requestRevokeAll() {
    pending = { kind: 'all' };
  }

  async function confirmPending() {
    const target = pending;
    if (!target) return;
    busy = true;
    try {
      if (target.kind === 'one') {
        await revokeMySession(target.id);
        toastStore.success('That device will be signed out within a few seconds.');
      } else {
        const n = await revokeMyOtherSessions();
        toastStore.success(
          n === 1 ? 'One other device signed out.' : `${n} other devices signed out.`,
        );
      }
      pending = null;
      // Drop BOTH cached lists, not just the one on screen. A revoke changes
      // what `include_revoked=0` and `include_revoked=1` each return, so
      // force-refetching only the visible key would leave the other one holding
      // pre-revoke rows — and the "Show recent sign-outs" toggle would then
      // present the session you just killed as still live, for up to a minute.
      viewCache.invalidate(SESSIONS_VIEW);
      await load(true);
    } catch (err) {
      // The backend's 409/400/404 bodies carry the actionable text — surface it
      // verbatim rather than a generic failure.
      toastStore.error(errorMessage(err));
    } finally {
      busy = false;
    }
  }

  function reasonLabel(reason: string | null): string {
    switch (reason) {
      case 'logout':
        return t('account.sessions.reason.logout');
      case 'user_revoked':
        return t('account.sessions.reason.userRevoked');
      case 'user_revoked_others':
        return t('account.sessions.reason.userRevokedOthers');
      case 'admin_revoked':
        return t('account.sessions.reason.adminRevoked');
      case 'password_changed':
        return t('account.sessions.reason.passwordChanged');
      case 'deactivated':
        return t('account.sessions.reason.deactivated');
      case 'reuse':
        return t('account.sessions.reason.reuse');
      default:
        return t('account.sessions.reason.ended');
    }
  }

  // `load()` builds its cache key synchronously, before its first `await`, so
  // reading `sessionStore.scopeKey` and `showRevoked` in `viewKey(...)` is what
  // registers them as this effect's dependencies. That is the whole dependency
  // list — do not "tidy" the key by hoisting it out of `load`.
  $effect(() => {
    void load();
  });
</script>

<AppShell requireProject={false}>
  <div class="head">
    <div>
      <h1 class="page-title">{t('account.title')}</h1>
      <p class="sub muted">{t('account.subtitle')}</p>
    </div>
    <!--
      Spins for a background revalidate too, not just an explicit click: that
      spinner IS the "showing cached data, fetching fresh" hint, and without it
      the instant paint is indistinguishable from live data. The click forces,
      because this button is also the only retry affordance on this page.
    -->
    <RefreshButton onclick={() => void load(true)} loading={loading || revalidating} />
  </div>

  {#if error}
    <div class="err-banner" role="alert">
      <Icon name="triangle-alert" size={15} />
      <span>{error}</span>
    </div>
  {/if}

  <div class="cards">
    <Card title={t('account.profile.title')}>
      <dl class="profile">
        <dt>{t('account.profile.name')}</dt>
        <dd>{authStore.user?.name || '—'}</dd>
        <dt>{t('account.profile.email')}</dt>
        <dd class="cell-mono">{authStore.user?.email ?? '—'}</dd>
        <dt>{t('account.profile.lastSignIn')}</dt>
        <dd><TimeValue value={authStore.user?.last_login_at} /></dd>
        <dt>{t('account.language.label')}</dt>
        <dd><LanguagePicker /></dd>
      </dl>
      <div class="profile-actions">
        <Button variant="secondary" href="#/change-password">
          {t('account.profile.changePassword')}
        </Button>
      </div>
    </Card>

    <Card title={t('account.sessions.title')} padding="none">
      {#snippet actions()}
        <Button variant="ghost" size="sm" onclick={() => void toggleHistory()}>
          {showRevoked ? t('account.sessions.hideRevoked') : t('account.sessions.showRevoked')}
        </Button>
        <Button
          variant="danger"
          size="sm"
          disabled={otherCount === 0 || !hasCurrent}
          onclick={requestRevokeAll}
        >
          {t('account.sessions.signOutOthers')}
        </Button>
      {/snippet}

      {#if !hasCurrent && !loading && live.length > 0}
        <div class="err-banner inset" role="status">
          <Icon name="info" size={15} />
          <span>{t('account.sessions.reloadHint')}</span>
        </div>
      {/if}

      {#if loading}
        <div class="center"><Spinner size={24} /></div>
      {:else if live.length === 0}
        <div class="pad">
          <EmptyState
            title={t('account.sessions.empty.title')}
            description={t('account.sessions.empty.description')}
          />
        </div>
      {:else}
        <DataTable>
          {#snippet head()}
            <tr>
              <SortableTh key="device" columnDefault="asc" sort={list.sort} {onsort}>
                {t('account.sessions.column.device')}
              </SortableTh>
              <SortableTh key="ip" columnDefault="asc" sort={list.sort} {onsort}>{t('account.sessions.column.ip')}</SortableTh>
              <SortableTh key="signed_in" sort={list.sort} {onsort}>{t('account.sessions.column.signedIn')}</SortableTh>
              <SortableTh key="last_used" sort={list.sort} {onsort}>{t('account.sessions.column.lastUsed')}</SortableTh>
              <!-- The revoke button (and, on a revoked row, the reason it
                   ended) — no value to order by. -->
              <th aria-label={t('common.actions')}></th>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each page.rows as s (s.id)}
              {#if s.revoked_at === null}
                <tr>
                  <td>
                    <span class="device">
                      {describeSession(s)}
                      {#if s.current}<Badge tone="primary" size="sm">{t('account.sessions.current')}</Badge>{/if}
                    </span>
                  </td>
                  <td class="cell-mono cell-muted">{s.ip ?? '—'}</td>
                  <td><TimeValue value={s.created_at} /></td>
                  <td><TimeValue value={s.last_used_at} /></td>
                  <td class="col-act">
                    {#if !s.current}
                      <Button
                        variant="ghost"
                        size="sm"
                        disabled={!hasCurrent}
                        onclick={() => requestRevokeOne(s)}
                      >
                        {t('account.sessions.revoke')}
                      </Button>
                    {/if}
                  </td>
                </tr>
              {:else}
                <tr class="dim">
                  <td>{describeSession(s)}</td>
                  <td class="cell-mono cell-muted">{s.ip ?? '—'}</td>
                  <td><TimeValue value={s.created_at} /></td>
                  <td>{t('account.sessions.signedOut')} <TimeValue value={s.revoked_at} /></td>
                  <td class="col-act cell-muted">{reasonLabel(s.revoked_reason)}</td>
                </tr>
              {/if}
            {/each}
          {/snippet}
        </DataTable>

        <!-- `total` is the length of the EXACT array handed to `pageSlice`
             above — `visible`, the same expression.

             Be accurate about why, because this table is NOT the pager rule's
             counter-example and was described as one in an earlier draft:
             `live` and `revoked` PARTITION `sessions`, so `visible.length` and
             `sessions.length` are always equal and measuring the wrong one
             could not produce a wrong answer here. Writing `visible.length` is
             a discipline — the rule is "measure the array you sliced", not
             "measure whichever array happens to agree today" — and it is what
             keeps this correct if a real filter is ever added above.

             The case where the two lengths genuinely differ is Inspector's
             findings tab, where each table is fed `g.findings`, a strict subset
             of `findings`; measuring the whole set there would offer an enabled
             Next onto an empty page, the bug that made `hasNext` a required
             prop on `Pagination` in slice 1.

             What this table DOES exercise is the rule's other half: the
             "Show recent sign-outs" toggle changes which rows the endpoint
             returns, and `toggleHistory` resets the offset with
             `setOffsetPage(list, 0)` so a narrowing change cannot strand you on
             a page that no longer exists. -->
        <ClientPager
          offset={list.offset}
          limit={PAGE}
          total={visible.length}
          onchange={(o) => (list = setOffsetPage(list, o))}
        />

        {#if proxied}
          <p class="hint muted">
            {t('account.sessions.proxyNote', { setting: 'API_TRUST_FORWARDED_HEADERS' })}
          </p>
        {/if}
      {/if}
    </Card>

    <NotificationSubscriptions />
  </div>
</AppShell>

<ConfirmDialog
  danger
  open={pending !== null}
  title={pending?.kind === 'all'
    ? t('account.confirm.revokeAll.title')
    : t('account.confirm.revokeOne.title')}
  message={pending?.kind === 'all'
    ? t('account.confirm.revokeAll.body')
    : t('account.confirm.revokeOne.body', {
        device:
          pending?.kind === 'one'
            ? pending.label
            : t('account.confirm.revokeOne.fallbackDevice'),
      })}
  confirmLabel={t('account.sessions.revoke')}
  loading={busy}
  onconfirm={() => void confirmPending()}
  oncancel={() => (pending = null)}
/>

<style>
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 18px;
  }
  .cards {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .profile {
    display: grid;
    grid-template-columns: 130px 1fr;
    row-gap: 8px;
    column-gap: 12px;
    margin: 0;
    font-size: 13.5px;
  }
  .profile dt {
    color: var(--text-faint);
  }
  .profile dd {
    margin: 0;
  }
  .profile-actions {
    margin-top: 16px;
  }
  .device {
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }
  .col-act {
    text-align: end;
    width: 1%;
    white-space: nowrap;
  }
  tr.dim td {
    opacity: 0.55;
  }
  .center {
    display: grid;
    place-items: center;
    padding: 36px 0;
  }
  .pad {
    padding: 18px;
  }
  .hint {
    margin: 10px 16px 14px;
    font-size: 12px;
  }
  .err-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 12px;
    margin-bottom: 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-2);
    font-size: 13px;
  }
  .err-banner.inset {
    margin: 14px 16px 0;
  }
</style>
