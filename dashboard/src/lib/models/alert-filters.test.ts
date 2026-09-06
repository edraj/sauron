import { describe, it, expect } from 'vitest';
import {
  withPinnedChoice,
  mergeEnvironmentNames,
  mergeStoredFilters,
  triggerNeeds,
  TRANSACTION_OPS,
  opLabel,
} from './alert-filters';

describe('withPinnedChoice', () => {
  it('leaves a list that already contains the selection untouched', () => {
    const options = ['production', 'staging'];
    expect(withPinnedChoice(options, 'staging')).toBe(options);
  });

  it('pins a saved value the offered list does not contain', () => {
    // A retired environment, or one from a project the session no longer has
    // selected. Without the pin the <select> falls back to its first option
    // and the rule silently loses its narrowing.
    expect(withPinnedChoice(['production'], 'retired-env')).toEqual([
      'retired-env',
      'production',
    ]);
  });

  it('pins an op outside the documented vocabulary', () => {
    // `op` is free-form on the wire — only the JS SDK coerces to `custom` —
    // so a rule can carry one TRANSACTION_OPS does not list.
    expect(withPinnedChoice([...TRANSACTION_OPS], 'grpc')).toEqual([
      'grpc',
      ...TRANSACTION_OPS,
    ]);
  });

  it('pins into an empty list rather than offering nothing', () => {
    // The 403-on-the-catalogue case with no app selected: the picker still has
    // to show what the rule is actually filtered to.
    expect(withPinnedChoice([], 'staging')).toEqual(['staging']);
  });

  it('never pins the empty selection', () => {
    // '' is the "any" case, which the dialog renders as its own fixed option —
    // pinning it would add a second, blank entry to every rule.
    expect(withPinnedChoice(['production'], '')).toEqual(['production']);
  });
});

describe('mergeEnvironmentNames', () => {
  it('dedupes the overlap between the catalogue and the session list', () => {
    expect(mergeEnvironmentNames(['production', 'staging'], ['production'])).toEqual([
      'production',
      'staging',
    ]);
  });

  it('keeps a session environment the catalogue call could not return', () => {
    // The 403 path: an org-scoped alert:write grant does not imply
    // project-scoped env:read, so the catalogue arm can come back empty.
    expect(mergeEnvironmentNames([], ['staging'])).toEqual(['staging']);
  });

  it('orders names so the picker is stable across loads', () => {
    expect(mergeEnvironmentNames(['staging', 'production'], ['dev'])).toEqual([
      'dev',
      'production',
      'staging',
    ]);
  });
});

describe('TRANSACTION_OPS', () => {
  it('matches the vocabulary the wire format documents', () => {
    // `envelope.rs`'s TransactionItem::op doc comment and the JS SDK's
    // TransactionOp union. A drift here offers a rule an op no SDK emits,
    // which counts zero forever and looks like a broken rule.
    expect([...TRANSACTION_OPS]).toEqual([
      'navigation',
      'http',
      'resource',
      'screen_load',
      'custom',
    ]);
  });
});

describe('opLabel', () => {
  it('renders the one underscored op as two words', () => {
    expect(opLabel('screen_load')).toBe('screen load');
  });

  it('leaves the single-word ops alone', () => {
    for (const op of ['navigation', 'http', 'resource', 'custom']) {
      expect(opLabel(op)).toBe(op);
    }
  });
});

describe('triggerNeeds', () => {
  const ALL = [
    'monitor_down',
    'monitor_up',
    'issue_new',
    'issue_regression',
    'error_threshold',
    'error_spike',
    'event_threshold',
    'perf_degradation',
  ];

  it('offers the environment filter on every trigger the evaluator polls', () => {
    // Mirrors `TriggerType::is_metric()`: everything except the two
    // prober-driven ones resolves `filters.environment` to enrollment ids.
    for (const t of ALL) {
      const monitor = t === 'monitor_down' || t === 'monitor_up';
      expect(triggerNeeds(t).env, t).toBe(!monitor);
    }
  });

  it('hides the environment filter on the monitor triggers', () => {
    // `monitors` has no `environment_id`, and `sauron-monitor` dispatches
    // these inline without ever reading `conditions` — so the field could
    // only ever have been decoration.
    expect(triggerNeeds('monitor_down').env).toBe(false);
    expect(triggerNeeds('monitor_up').env).toBe(false);
  });

  it('offers the operation filter only on the latency trigger', () => {
    for (const t of ALL) {
      expect(triggerNeeds(t).op, t).toBe(t === 'perf_degradation');
    }
  });

  it('pairs the monitor picker with the two monitor triggers and nothing else', () => {
    for (const t of ALL) {
      const monitor = t === 'monitor_down' || t === 'monitor_up';
      expect(triggerNeeds(t).monitor, t).toBe(monitor);
      // The window is the mirror image, and both come from the same flag —
      // a regression that inverts one inverts the other.
      expect(triggerNeeds(t).window, t).toBe(!monitor);
    }
  });

  it('offers a metric only where one is measured', () => {
    for (const t of ALL) {
      expect(triggerNeeds(t).metric, t).toBe(t === 'perf_degradation');
    }
  });
});

describe('mergeStoredFilters', () => {
  const needs = (t: string) => triggerNeeds(t);

  it('keeps a hidden field’s stored key instead of deleting it', () => {
    // The monitor case: `environment` is inert on a prober-driven trigger, but
    // inert is not unwanted — renaming the rule must not clear it.
    expect(
      mergeStoredFilters({}, { environment: 'staging' }, needs('monitor_down')),
    ).toEqual({ environment: 'staging' });
  });

  it('keeps keys no field owns at all', () => {
    // `tag_key`/`tag_value` are read by the evaluator and narrow a real count.
    // Nothing in the dialog can show them, so a save that dropped them would
    // silently change what the rule counts.
    expect(
      mergeStoredFilters(
        { level: 'error' },
        { level: 'error', tag_key: 'tier', tag_value: 'gold' },
        needs('error_threshold'),
      ),
    ).toEqual({ level: 'error', tag_key: 'tier', tag_value: 'gold' });
  });

  it('lets the dialog clear a filter whose field it IS showing', () => {
    // The other half, and the one a naive "preserve everything" breaks:
    // emptying a visible field has to mean something.
    expect(
      mergeStoredFilters({}, { environment: 'staging' }, needs('error_threshold')),
    ).toEqual({});
  });

  it('lets the dialog change a filter whose field it is showing', () => {
    expect(
      mergeStoredFilters(
        { environment: 'production' },
        { environment: 'staging' },
        needs('error_threshold'),
      ),
    ).toEqual({ environment: 'production' });
  });

  it('preserves the op of a rule edited under a trigger that hides it', () => {
    // `op` is offered only on perf_degradation; an API-made rule can carry one
    // anywhere.
    expect(mergeStoredFilters({}, { op: 'http' }, needs('event_threshold'))).toEqual({
      op: 'http',
    });
  });

  it('is a no-op for a new rule, which has nothing stored', () => {
    expect(mergeStoredFilters({ environment: 'staging' }, {}, needs('perf_degradation'))).toEqual({
      environment: 'staging',
    });
  });

  it('ignores stored keys whose value is empty', () => {
    // An empty string round-tripped from the API is not a filter; writing it
    // back would make `Filters::from_value`’s own `.filter(|s| !s.is_empty())`
    // the only thing standing between it and a query.
    expect(mergeStoredFilters({}, { environment: '' }, needs('monitor_down'))).toEqual({});
  });
});
