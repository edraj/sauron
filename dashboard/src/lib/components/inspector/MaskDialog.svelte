<!--
  Mask confirmation. Modal size="md" — ConfirmDialog has no text input, and
  typing the APP SLUG is the only confirmation that forces attention onto the
  thing that actually goes wrong.

  The §1 "what this does not reach" panel is PERMANENTLY VISIBLE and
  non-collapsible. The product must never say "permanently removed": masking
  rewrites rows in hot Postgres only.
-->
<script lang="ts">
  import { untrack } from 'svelte';
  import Modal from '../ui/Modal.svelte';
  import Button from '../ui/Button.svelte';
  import Input from '../ui/Input.svelte';
  import Badge from '../ui/Badge.svelte';
  import Spinner from '../ui/Spinner.svelte';
  import * as inspectorApi from '../../api/inspector';
  import { errorMessage } from '../../api/client';
  import { toastStore } from '../../stores/toast.svelte';
  import {
    UNREACHABLE_COPY,
    describeTarget,
    expandCompanionTargets,
    maskConfirmReady,
  } from '../../models/inspector';
  import type { InspectorFinding, InspectorMaskAction } from '../../models';

  interface Props {
    appId: string;
    finding: InspectorFinding;
    onclose: () => void;
    ondone: () => void;
  }
  const { appId, finding, onclose, ondone }: Props = $props();

  // $state.raw: the action is replaced wholesale on every poll, and deep
  // proxying it means `action === previous` never matches, which would restart
  // the poll effect on every tick.
  let action = $state.raw<InspectorMaskAction | null>(null);
  let slug = $state('');
  let ttlSecs = $state(900);
  let maxRows = $state(20_000_000);
  let latencySecs = $state(30);
  let typed = $state('');
  let starting = $state(true);
  let submitting = $state(false);
  let error = $state('');

  // Computed locally so the blast radius is described BEFORE the server
  // answers. Mirrors the backend's expand_targets.
  const previewTargets = $derived(
    expandCompanionTargets({
      table: finding.source_table,
      column: finding.source_column,
      path: finding.key_path,
    }),
  );
  const touchesEventUser = $derived(previewTargets.some((t) => t.column === 'event_user'));

  // A ticking clock, because `maskConfirmReady` reads `Date.now()` and a clock
  // is not reactive. Without it `ready` recomputes only when `typed`/`action`
  // change — and the poll STOPS the moment the preview completes — so the
  // button froze in whatever state the last keystroke left it. Measured: typed
  // the slug 6 s after the preview landed, waited, and the button was still
  // enabled 169 s into a 60 s TTL; clicking it returned 409 "the preview is not
  // ready or has expired". The server gate is right and the UI was lying about
  // it.
  let nowTick = $state(Date.now());
  $effect(() => {
    const id = setInterval(() => (nowTick = Date.now()), 1000);
    return () => clearInterval(id);
  });

  const ready = $derived.by(() => {
    // Read the tick so this re-runs once a second; the value itself is unused
    // because `maskConfirmReady` asks the clock again itself.
    void nowTick;
    if (!action) return false;
    return maskConfirmReady(
      typed,
      slug,
      {
        status: action.status,
        previewed_at: action.previewed_at,
        estimated_rows: action.estimated_rows,
      },
      ttlSecs,
      maxRows,
    );
  });

  // A confirm button that goes quiet with the right slug typed reads as a bug
  // unless the dialog says why.
  const expired = $derived.by(() => {
    void nowTick;
    const at = action?.previewed_at;
    if (!action || action.status !== 'previewed' || !at) return false;
    return (Date.now() - Date.parse(at)) / 1000 > ttlSecs;
  });

  $effect(() => {
    // Prop-seeding read, wrapped in untrack() so a parent reload cannot wipe a
    // half-typed confirmation by re-running this effect.
    const id = untrack(() => appId);
    const f = untrack(() => finding);
    void (async () => {
      try {
        const started = await inspectorApi.maskPreview(id, { finding_id: f.id });
        action = started.action;
        slug = started.app_slug;
        ttlSecs = started.preview_ttl_secs;
        maxRows = started.mask_max_rows;
        latencySecs = started.enforcement_latency_secs;
      } catch (e) {
        // errorMessage, not `e instanceof Error`: the API client rejects with a
        // plain {status,code,message} object, which stringifies to
        // "[object Object]" and hides the 403 this dialog exists to surface.
        error = errorMessage(e);
      } finally {
        starting = false;
      }
    })();
  });

  $effect(() => {
    const a = action;
    if (!a || a.status !== 'preview') return;
    const id = setInterval(async () => {
      try {
        action = await inspectorApi.getMaskAction(a.id);
      } catch {
        // A transient poll failure must not close the dialog; the next tick
        // retries and the confirm button stays disabled meanwhile.
      }
    }, 2000);
    return () => clearInterval(id);
  });

  async function confirm() {
    if (!action) return;
    submitting = true;
    try {
      await inspectorApi.confirmMask(action.id, typed.trim());
      toastStore.success(
        `Mask queued. New events are masked within about ${latencySecs} seconds.`,
      );
      ondone();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      submitting = false;
    }
  }
</script>

<!-- `open` is required and defaults to FALSE, so a Modal mounted without it
     never calls showModal(). The parent mounts this component only while a
     finding is selected, so open is a constant here. -->
<Modal open size="md" title="Mask this value" {onclose}>
  {#if error}
    <p class="err">{error}</p>
  {/if}

  <h4>What will be rewritten</h4>
  <ul>
    {#each previewTargets as t, i (i)}
      <li><code>{describeTarget(t)}</code></li>
    {/each}
  </ul>
  <p class="note">
    The value becomes the JSON string <code>"****"</code> and the key is kept. The TYPE changes, so
    arithmetic, containment filters and range comparisons stop working for masked rows.
  </p>
  {#if touchesEventUser}
    <p class="warn">
      This masks <code>event_user</code>, which backs the <code>user.email:</code> search dimension.
      Masked rows will silently stop matching those queries.
    </p>
  {/if}

  <h4>Affected rows</h4>
  {#if starting || !action || action.status === 'preview'}
    <p><Spinner size={14} /> Counting affected rows…</p>
  {:else if action.status === 'previewed'}
    <p class="counts">
      <Badge>{action.estimated_rows.toLocaleString()} rows</Badge>
      <Badge tone="neutral"
        >{action.cold_rows_skipped.toLocaleString()} row(s) already in cold storage, skipped</Badge
      >
    </p>
    <p class="note">
      The count was taken a moment ago. On an actively ingesting app more rows will match by the
      time the mask runs, so a larger "rows masked" figure afterwards is normal, not an error.
    </p>
  {:else}
    <p class="err">The preview did not complete: {action.error || action.status}</p>
  {/if}

  <h4>What this does not reach</h4>
  <div class="unreachable">
    {#each UNREACHABLE_COPY as r, i (i)}
      <p class:headline={r.headline} class:readFirst={r.readFirst}>
        {#if r.headline}
          <!-- The headline's `what` IS the sentence that must never be
               softened into "permanently removed"; rendering only `why` left it
               dangling without its subject. -->
          <strong>{r.what}</strong>
          {r.why}
        {:else}
          <strong>{r.what}</strong> — {r.why}
          <span class="bounded">Bounded by: {r.bounded}</span>
        {/if}
      </p>
    {/each}
    <!--
      Verbatim from wiki/Active-Users.md § "Two things that silently change
      these numbers". The active-users report and this dialog must not describe
      the same consequence in two different ways: masking an identity key
      changes the identified/guest split with nothing to notice afterwards, and
      the operator reading THIS panel is the last person who can decide.
    -->
    <p class="wiki">
      <strong>A PII mask on an identity-bearing key dismantles cross-app matching.</strong> The mask
      enforcer runs before identification, so once <code>context.user.id</code> — or whatever key
      your app uses as its <code>distinct_id</code>; an email address is both a common choice and
      exactly the kind of value a PII policy flags — is masked, no future person can ever be marked
      identified through it. Nobody already identified loses the flag, so nothing moves on the day
      the mask lands: instead <strong>Identified</strong> plateaus and then decays as the existing
      population churns, while <strong>Guests</strong> climbs to meet it. Nothing labels the cause
      and nothing can reconstruct it afterwards. Decide before you apply the mask, not after.
      <span class="bounded">
        wiki/Active-Users.md — the <a href="#/active-users">Active users</a> report is where this
        shows up, and it shows up as a trend with no event on it.
      </span>
    </p>
  </div>
  <p class="note">A running mask can be stopped, but it cannot be undone. There is no shadow copy.</p>

  <label for="mask-confirm">Type the app slug ({slug}) to confirm</label>
  <Input id="mask-confirm" bind:value={typed} placeholder={slug} autocomplete="off" />
  {#if expired}
    <p class="warn">
      This preview is older than {ttlSecs} seconds and no longer counts as fresh. Close this dialog
      and start again — the count would be stale for a decision that cannot be undone.
    </p>
  {/if}

  {#snippet footer()}
    <Button onclick={onclose}>Cancel</Button>
    <Button variant="danger" disabled={!ready || submitting} onclick={confirm}>
      {submitting ? 'Queuing…' : 'Mask permanently in hot Postgres'}
    </Button>
  {/snippet}
</Modal>

<style>
  /* --danger is not defined in app.css; the theme name is --error. An undefined
     custom property with no fallback invalidates the whole declaration, so the
     failure text would silently inherit body colour and stop reading as a
     failure — which is exactly how a 403 on this dialog would go unnoticed. */
  .err {
    color: var(--error);
  }
  .warn {
    color: var(--warning);
    font-size: 12.5px;
  }
  .note {
    color: var(--text-muted);
    font-size: 12.5px;
  }
  h4 {
    margin-top: 14px;
    font-size: 13px;
    font-weight: 620;
  }
  ul {
    margin: 6px 0;
    padding-left: 18px;
    font-size: 12.5px;
  }
  .counts {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-bottom: 6px;
  }
  .unreachable {
    max-height: 260px;
    overflow-y: auto;
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px 10px;
    font-size: 12.5px;
    color: var(--text-muted);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .unreachable .headline {
    font-weight: 600;
    color: var(--text);
  }
  .unreachable .readFirst,
  .unreachable .wiki {
    border-left: 3px solid var(--warning);
    padding-left: 8px;
  }
  .bounded {
    display: block;
    color: var(--text-muted);
  }
  label {
    display: block;
    margin-top: 12px;
    font-size: 13px;
    font-weight: 550;
  }
</style>
