/// Workflows: named, explicitly-bounded spans of activity an app declares via
/// `startWorkflow` / `endWorkflow` / `cancelWorkflow` on [SauronClient]. While
/// one is active, its id/name are stamped onto every error/event/transaction
/// and three reserved lifecycle analytics events (`$workflow_start`,
/// `$workflow_end`, `$workflow_cancel`) are emitted around it — see the
/// stamping and method bodies in `client.dart`.
library;

/// Cap on a workflow name, after trimming.
const int kWorkflowNameMax = 120;

/// Cap on a cancel reason, after trimming.
const int kWorkflowReasonMax = 120;

/// The six terminal outcomes of a workflow call. No seventh value — see
/// `disabled`'s doc for why it also covers an unexpected internal error.
enum WorkflowStatus {
  /// The call took effect as requested.
  ok,

  /// `startWorkflow` was called while another workflow is already active and
  /// `force` was not set.
  alreadyActive,

  /// `endWorkflow`/`cancelWorkflow` was called with no workflow active.
  notActive,

  /// `endWorkflow`/`cancelWorkflow` was called with an explicit `name` that
  /// does not match the active workflow's (name normalization failures on
  /// end/cancel land here too, not `invalidName`).
  nameMismatch,

  /// `startWorkflow`'s `name` was empty (after trimming) or over 120 chars.
  invalidName,

  /// The SDK did not perform the call — before `init`, after `close()`, after
  /// the transport auto-disabled itself (401/403), or an unexpected internal
  /// error. Never a claim about workflow state or caller input.
  disabled,
}

/// The outcome of a workflow call: the [status], plus the workflow id on
/// success (`start`/`end`/`cancel` all return the id of the workflow they
/// affected when `status == ok`).
class WorkflowResult {
  const WorkflowResult(this.status, [this.workflowId]);

  final WorkflowStatus status;
  final String? workflowId;
}

/// The currently active workflow: a client-generated id paired with its name
/// and start time. Always held as a single value — `workflowId`/`name` are
/// never modeled as two independent nullable fields, so they can never be
/// stamped or emitted one without the other.
class ActiveWorkflow {
  ActiveWorkflow({
    required this.workflowId,
    required this.name,
    required this.startedAt,
  });

  /// Fresh client-generated UUID v4, minted by `startWorkflow`. Never a
  /// session id, device id, or anything derived from the name — the server's
  /// rollup key is `(app_id, workflow_id)` app-wide, so a reused/deterministic
  /// id would merge counters across unrelated environments/sessions.
  final String workflowId;

  /// The trimmed, validated workflow name.
  final String name;

  /// When the workflow started (UTC) — used to compute `duration_ms` on close.
  final DateTime startedAt;
}

/// Returns the trimmed name, or `null` when invalid (empty after trimming, or
/// over [kWorkflowNameMax] chars). Order matters: trim, then check emptiness,
/// then check length — all against the trimmed value.
String? normalizeWorkflowName(String? name) {
  if (name == null) {
    return null;
  }
  final String trimmed = name.trim();
  if (trimmed.isEmpty || trimmed.length > kWorkflowNameMax) {
    return null;
  }
  return trimmed;
}

/// Normalizes a cancel reason: defaults to `'user'` when null/blank, else
/// trims and caps at [kWorkflowReasonMax]. Never rejects — a reason is
/// metadata, not a precondition.
String normalizeWorkflowReason(String? reason) {
  final String trimmed = reason?.trim() ?? '';
  if (trimmed.isEmpty) {
    return 'user';
  }
  return trimmed.length > kWorkflowReasonMax
      ? trimmed.substring(0, kWorkflowReasonMax)
      : trimmed;
}
