"""Workflows: named, explicitly-bounded spans of activity.

Exercises `start_workflow`/`end_workflow`/`cancel_workflow`/`get_workflow`
through the module-level API (`sauron.init` + the fake transport), plus one
`Client`-level test that targets the "disabled but the object is still
reachable" precondition directly.

The mandatory test is `test_workflow_does_not_leak_across_concurrent_asyncio_tasks`:
it drives two `asyncio` tasks concurrently on ONE thread, each starting its
own workflow and yielding control (`await asyncio.sleep`) before tracking —
this could not pass if the active workflow were a module-level global (a bare
global is shared process-wide; a second task's `start_workflow` would stomp
the first's before it resumes and tracks). It only passes because the active
workflow lives on `Scope.workflow`, isolated per `contextvars.ContextVar`
context (see `sauron/_scope.py`), and each `asyncio.gather` argument runs in
its own copied context once `with sauron.scope():` pushes one.
"""

import asyncio
import threading
import unittest
import uuid

import sauron
from sauron._client import Client
from sauron._scope import get_current_scope, reset_scopes
from sauron._workflow import WORKFLOW_NAME_MAX, WORKFLOW_REASON_MAX

from ._fake import FakeSender

DSN = "https://pk_test@localhost:8081/1"


def _by_type(items):
    out = {}
    for item in items:
        out.setdefault(item["type"], []).append(item)
    return out


def _active_workflow():
    """``sauron.get_workflow()``, asserted non-``None``.

    Uses a bare ``assert`` rather than ``assertIsNotNone`` because only the
    former narrows ``Optional[ActiveWorkflow]`` for a type checker — pyright
    reports ``reportOptionalMemberAccess`` on every ``.name``/``.workflow_id``
    that follows an ``assertIsNotNone``.
    """
    wf = sauron.get_workflow()
    assert wf is not None
    return wf


class WorkflowTests(unittest.TestCase):
    def setUp(self):
        reset_scopes()
        self.sender = FakeSender(status=200)

    def tearDown(self):
        sauron.close(timeout=2)
        reset_scopes()

    def _init(self, **kwargs):
        return sauron.init(
            DSN, flush_interval=3600, max_batch=1000, sender=self.sender,
            **kwargs,
        )

    # -- start_workflow ------------------------------------------------------

    def test_start_emits_workflow_start_stamped(self):
        self._init()
        result = sauron.start_workflow("checkout")
        self.assertEqual(result.status, sauron.WorkflowStatus.OK)
        assert result.workflow_id is not None
        # A real UUID4, not a session id / name hash / anything deterministic.
        parsed = uuid.UUID(result.workflow_id)
        self.assertEqual(parsed.version, 4)

        sauron.flush()
        items = self.sender.items
        self.assertEqual(len(items), 1)
        start_event = items[0]
        self.assertEqual(start_event["type"], "event")
        self.assertEqual(start_event["name"], "$workflow_start")
        # Item-level fields...
        self.assertEqual(start_event["workflow_id"], result.workflow_id)
        self.assertEqual(start_event["workflow_name"], "checkout")
        # ...and also in `properties`, for the hand-rolled-client fallback.
        self.assertEqual(
            start_event["properties"]["workflow_id"], result.workflow_id
        )
        self.assertEqual(start_event["properties"]["workflow_name"], "checkout")

        wf = _active_workflow()
        self.assertEqual(wf.name, "checkout")
        self.assertEqual(wf.workflow_id, result.workflow_id)

    def test_start_trims_the_name(self):
        self._init()
        result = sauron.start_workflow("  checkout  ")
        self.assertEqual(result.status, sauron.WorkflowStatus.OK)
        self.assertEqual(_active_workflow().name, "checkout")

    def test_rejects_empty_and_overlong_names(self):
        self._init()
        self.assertEqual(
            sauron.start_workflow("").status, sauron.WorkflowStatus.INVALID_NAME
        )
        self.assertEqual(
            sauron.start_workflow("   ").status,
            sauron.WorkflowStatus.INVALID_NAME,
        )
        self.assertEqual(
            sauron.start_workflow("x" * (WORKFLOW_NAME_MAX + 1)).status,
            sauron.WorkflowStatus.INVALID_NAME,
        )
        # Boundary: exactly the cap is accepted.
        ok = sauron.start_workflow("x" * WORKFLOW_NAME_MAX)
        self.assertEqual(ok.status, sauron.WorkflowStatus.OK)
        # None of the rejected calls left a workflow active or emitted
        # anything.
        sauron.end_workflow()
        sauron.flush()
        names = [i["name"] for i in self.sender.items if i["type"] == "event"]
        self.assertEqual(names, ["$workflow_start", "$workflow_end"])

    def test_start_while_active_returns_already_active_and_emits_nothing(self):
        self._init()
        first = sauron.start_workflow("checkout")
        self.assertEqual(first.status, sauron.WorkflowStatus.OK)

        second = sauron.start_workflow("other")
        self.assertEqual(second.status, sauron.WorkflowStatus.ALREADY_ACTIVE)
        self.assertIsNone(second.workflow_id)
        # The original workflow is untouched.
        self.assertEqual(_active_workflow().name, "checkout")

        sauron.flush()
        names = [i["name"] for i in self.sender.items if i["type"] == "event"]
        self.assertEqual(names, ["$workflow_start"])

    def test_force_cancels_with_superseded_then_starts_new(self):
        self._init()
        first = sauron.start_workflow("checkout")
        second = sauron.start_workflow("refund", force=True)
        self.assertEqual(second.status, sauron.WorkflowStatus.OK)
        self.assertNotEqual(second.workflow_id, first.workflow_id)
        self.assertEqual(_active_workflow().name, "refund")

        sauron.flush()
        events = [i for i in self.sender.items if i["type"] == "event"]
        self.assertEqual(
            [e["name"] for e in events], ["$workflow_start", "$workflow_cancel", "$workflow_start"]
        )
        cancel_event = events[1]
        self.assertEqual(cancel_event["workflow_id"], first.workflow_id)
        self.assertEqual(cancel_event["workflow_name"], "checkout")
        self.assertEqual(cancel_event["properties"]["reason"], "superseded")
        new_start = events[2]
        self.assertEqual(new_start["workflow_id"], second.workflow_id)
        self.assertEqual(new_start["workflow_name"], "refund")

    # -- end_workflow ----------------------------------------------------

    def test_end_emits_workflow_end_with_duration_and_clears(self):
        self._init()
        start = sauron.start_workflow("checkout")
        result = sauron.end_workflow()
        self.assertEqual(result.status, sauron.WorkflowStatus.OK)
        self.assertEqual(result.workflow_id, start.workflow_id)
        self.assertIsNone(sauron.get_workflow())

        sauron.flush()
        end_event = self.sender.items[-1]
        self.assertEqual(end_event["name"], "$workflow_end")
        self.assertEqual(end_event["workflow_id"], start.workflow_id)
        self.assertEqual(end_event["workflow_name"], "checkout")
        self.assertIn("duration_ms", end_event["properties"])
        self.assertGreaterEqual(end_event["properties"]["duration_ms"], 0)
        # `$workflow_end` never carries a `reason`.
        self.assertNotIn("reason", end_event["properties"])

    def test_end_with_matching_explicit_name_ok(self):
        self._init()
        sauron.start_workflow("checkout")
        result = sauron.end_workflow("checkout")
        self.assertEqual(result.status, sauron.WorkflowStatus.OK)

    def test_end_with_mismatched_name_is_noop(self):
        self._init()
        sauron.start_workflow("checkout")
        result = sauron.end_workflow("something-else")
        self.assertEqual(result.status, sauron.WorkflowStatus.NAME_MISMATCH)
        # Still active — untouched.
        self.assertEqual(_active_workflow().name, "checkout")
        sauron.flush()
        names = [i["name"] for i in self.sender.items if i["type"] == "event"]
        self.assertEqual(names, ["$workflow_start"])

    def test_end_with_malformed_explicit_name_is_name_mismatch_not_invalid_name(self):
        self._init()
        sauron.start_workflow("checkout")
        result = sauron.end_workflow("   ")
        self.assertEqual(result.status, sauron.WorkflowStatus.NAME_MISMATCH)
        result2 = sauron.end_workflow("x" * (WORKFLOW_NAME_MAX + 1))
        self.assertEqual(result2.status, sauron.WorkflowStatus.NAME_MISMATCH)
        self.assertEqual(_active_workflow().name, "checkout")

    def test_end_with_none_active_returns_not_active(self):
        self._init()
        result = sauron.end_workflow()
        self.assertEqual(result.status, sauron.WorkflowStatus.NOT_ACTIVE)
        sauron.flush()
        self.assertEqual(len(self.sender.items), 0)

    # -- cancel_workflow ---------------------------------------------------

    def test_cancel_defaults_reason_to_user_and_caps_at_120(self):
        self._init()
        sauron.start_workflow("checkout")
        result = sauron.cancel_workflow()
        self.assertEqual(result.status, sauron.WorkflowStatus.OK)
        sauron.flush()
        cancel_event = self.sender.items[-1]
        self.assertEqual(cancel_event["name"], "$workflow_cancel")
        self.assertEqual(cancel_event["properties"]["reason"], "user")

        self.sender.calls.clear()
        sauron.start_workflow("checkout-2")
        long_reason = "x" * 300
        sauron.cancel_workflow(reason=long_reason)
        sauron.flush()
        cancel_event2 = [
            i for i in self.sender.items if i["name"] == "$workflow_cancel"
        ][0]
        self.assertEqual(len(cancel_event2["properties"]["reason"]), WORKFLOW_REASON_MAX)
        self.assertEqual(cancel_event2["properties"]["reason"], "x" * WORKFLOW_REASON_MAX)

    def test_cancel_with_none_active_returns_not_active(self):
        self._init()
        result = sauron.cancel_workflow()
        self.assertEqual(result.status, sauron.WorkflowStatus.NOT_ACTIVE)

    def test_cancel_with_mismatched_name_is_noop(self):
        self._init()
        sauron.start_workflow("checkout")
        result = sauron.cancel_workflow("something-else")
        self.assertEqual(result.status, sauron.WorkflowStatus.NAME_MISMATCH)
        self.assertEqual(_active_workflow().name, "checkout")

    # -- stamping across every construction path ----------------------------

    def test_stamps_track_capture_exception_capture_message_and_transaction(self):
        self._init()
        start = sauron.start_workflow("checkout")

        sauron.track("added_to_cart", "u_1", {})
        try:
            raise ValueError("boom")
        except ValueError:
            sauron.capture_exception()
        # `capture_message` builds its error item inline (no shared builder) —
        # this is the exact path a stamp is easiest to forget (contract item 17).
        sauron.capture_message("inline built message")
        sauron.track_transaction("GET /x", duration_ms=12.0)
        sauron.identify("u_1", {"plan": "pro"})

        sauron.flush()
        by_type = _by_type(self.sender.items)

        for ev in by_type["event"]:
            self.assertEqual(ev["workflow_id"], start.workflow_id)
            self.assertEqual(ev["workflow_name"], "checkout")
        self.assertEqual(
            sorted(e["name"] for e in by_type["event"]),
            sorted(["$workflow_start", "added_to_cart"]),
        )

        self.assertEqual(len(by_type["error"]), 2)
        for err in by_type["error"]:
            self.assertEqual(err["workflow_id"], start.workflow_id)
            self.assertEqual(err["workflow_name"], "checkout")

        self.assertEqual(len(by_type["transaction"]), 1)
        self.assertEqual(by_type["transaction"][0]["workflow_id"], start.workflow_id)
        self.assertEqual(by_type["transaction"][0]["workflow_name"], "checkout")

        # `identify` is never stamped — no workflow columns for it.
        self.assertEqual(len(by_type["identify"]), 1)
        self.assertNotIn("workflow_id", by_type["identify"][0])
        self.assertNotIn("workflow_name", by_type["identify"][0])

    def test_keys_absent_when_no_workflow(self):
        """The omission test: no workflow ever started, on every item type.

        Uses `assertNotIn` (dict key membership), not an equality/`None`
        check — that is what actually distinguishes "key absent" from "key
        present with value `None`", the trap this contract explicitly calls
        out (a Python dict has no `undefined`-vs-absent ambiguity the way JS
        does, but the wrong implementation — `item["workflow_id"] = None` —
        would still slip past a `.get(...) is None` style assertion).
        """
        self._init()
        sauron.track("no_workflow_event", "u_1", {})
        try:
            raise RuntimeError("bare")
        except RuntimeError:
            sauron.capture_exception()
        sauron.capture_message("no workflow message")
        sauron.track_transaction("GET /y", duration_ms=5.0)
        sauron.identify("u_1", {})
        sauron.flush()

        self.assertTrue(self.sender.items)
        for item in self.sender.items:
            self.assertNotIn("workflow_id", item)
            self.assertNotIn("workflow_name", item)

    # -- disabled ------------------------------------------------------------

    def test_disabled_before_init(self):
        for result in (
            sauron.start_workflow("checkout"),
            sauron.end_workflow(),
            sauron.cancel_workflow(),
        ):
            self.assertEqual(result.status, sauron.WorkflowStatus.DISABLED)
        self.assertIsNone(sauron.get_workflow())

    def test_disabled_after_close(self):
        self._init()
        started = sauron.start_workflow("checkout")
        self.assertEqual(started.status, sauron.WorkflowStatus.OK)
        sauron.close()
        result = sauron.start_workflow("other")
        self.assertEqual(result.status, sauron.WorkflowStatus.DISABLED)


class WorkflowFailurePathTests(unittest.TestCase):
    """Failures that occur AFTER state has been mutated must never be
    reported as ``disabled``.

    Contract item 15's closing invariant: "``disabled`` from the catch only
    ever means 'failed before touching state'." Each test here forces a
    throw at a specific point and asserts the returned status is still
    truthful about what actually happened to the workflow.
    """

    def setUp(self):
        reset_scopes()
        self.sender = FakeSender(status=200)
        self.client = Client(
            DSN, flush_interval=3600, max_batch=1000, sender=self.sender
        )

    def tearDown(self):
        self.client.close(timeout=2)
        reset_scopes()

    def _events(self):
        self.client.flush()
        return [i for i in self.sender.items if i["type"] == "event"]

    def test_force_supersede_does_not_destroy_the_old_workflow_then_report_disabled(self):
        """C1 / addendum 23: the replacement id+timestamp are minted BEFORE
        the supersede close, so a throw while minting cannot leave the old
        workflow already cancelled-and-cleared behind a ``disabled`` return.
        """
        first = self.client.start_workflow("checkout")
        self.assertEqual(first.status, sauron.WorkflowStatus.OK)

        # Break uuid4 exactly the way the reviewer did.
        original_uuid4 = uuid.uuid4
        uuid.uuid4 = lambda: (_ for _ in ()).throw(RuntimeError("uuid is down"))
        try:
            result = self.client.start_workflow("refund", force=True)
        finally:
            uuid.uuid4 = original_uuid4

        # `disabled` is the correct status here — but ONLY because it is now
        # truthful: nothing may have been mutated or emitted.
        self.assertEqual(result.status, sauron.WorkflowStatus.DISABLED)

        # The pre-existing workflow must survive intact...
        surviving = get_current_scope().workflow
        assert surviving is not None
        self.assertEqual(surviving.name, "checkout")
        self.assertEqual(surviving.workflow_id, first.workflow_id)

        # ...and NO `$workflow_cancel(reason="superseded")` may have escaped
        # to the wire. This is the exact assertion that fails if the mint
        # happens after `_emit_workflow_close`.
        names = [e["name"] for e in self._events()]
        self.assertEqual(names, ["$workflow_start"])
        self.assertNotIn("$workflow_cancel", names)

    def test_end_returns_ok_when_the_closing_emit_raises(self):
        """C2 / item 15(a): the state was cleared, so the caller must be
        told ``ok`` — never ``disabled``, which would claim nothing
        happened."""
        start = self.client.start_workflow("checkout")

        original_track = self.client.track
        self.client.track = lambda *a, **k: (_ for _ in ()).throw(
            RuntimeError("transport exploded")
        )
        try:
            result = self.client.end_workflow()
        finally:
            self.client.track = original_track

        self.assertEqual(result.status, sauron.WorkflowStatus.OK)
        self.assertEqual(result.workflow_id, start.workflow_id)
        # State was in fact cleared — which is precisely why `ok` (not
        # `disabled`) is the honest answer.
        self.assertIsNone(get_current_scope().workflow)

    def test_cancel_returns_ok_when_the_closing_emit_raises(self):
        start = self.client.start_workflow("checkout")

        original_track = self.client.track
        self.client.track = lambda *a, **k: (_ for _ in ()).throw(
            RuntimeError("transport exploded")
        )
        try:
            result = self.client.cancel_workflow(reason="bye")
        finally:
            self.client.track = original_track

        self.assertEqual(result.status, sauron.WorkflowStatus.OK)
        self.assertEqual(result.workflow_id, start.workflow_id)
        self.assertIsNone(get_current_scope().workflow)

    def test_start_returns_ok_when_the_start_emit_raises(self):
        """The already-correct parallel case, pinned so it cannot regress:
        the workflow IS live locally, so `ok` + the id, not `disabled`."""
        original_track = self.client.track
        self.client.track = lambda *a, **k: (_ for _ in ()).throw(
            RuntimeError("transport exploded")
        )
        try:
            result = self.client.start_workflow("checkout")
        finally:
            self.client.track = original_track

        self.assertEqual(result.status, sauron.WorkflowStatus.OK)
        self.assertIsNotNone(result.workflow_id)
        live = get_current_scope().workflow
        assert live is not None
        self.assertEqual(live.workflow_id, result.workflow_id)


class WorkflowAnonymousDistinctIdTests(unittest.TestCase):
    """I3: an anonymous workflow run sends ``distinct_id=""`` — never a
    synthetic sentinel (device id / "system" / "anon_<id>").

    Empty is what the server was built for: the ``bump_workflow`` /
    ``apply_workflow_lifecycle`` call sites filter empty to ``NULL``
    (``process.rs:381``, ``:440``) and ``unique_users`` is
    ``COUNT(DISTINCT w.distinct_id)`` (``repo.rs:3162``), which skips
    ``NULL``s — so an anonymous run contributes nothing rather than
    collapsing every anonymous run onto one fake user.
    """

    def setUp(self):
        reset_scopes()
        self.sender = FakeSender(status=200)
        self.client = Client(
            DSN, flush_interval=3600, max_batch=1000, sender=self.sender
        )

    def tearDown(self):
        self.client.close(timeout=2)
        reset_scopes()

    def _lifecycle_events(self):
        self.client.flush()
        return [
            i
            for i in self.sender.items
            if i["type"] == "event" and i["name"].startswith("$workflow_")
        ]

    def test_lifecycle_events_use_empty_distinct_id_when_anonymous(self):
        self.client.start_workflow("checkout")
        self.client.end_workflow()

        events = self._lifecycle_events()
        self.assertEqual(
            [e["name"] for e in events], ["$workflow_start", "$workflow_end"]
        )
        for event in events:
            # Present on the wire (the field is a required String), and empty
            # — not absent, not a device id, not any other sentinel.
            self.assertIn("distinct_id", event)
            self.assertEqual(event["distinct_id"], "")

    def test_anonymous_lifecycle_distinct_id_is_not_the_device_id(self):
        """Guards against a regression back to a synthetic per-process id."""
        self.client.start_workflow("checkout")
        self.client.cancel_workflow()
        for event in self._lifecycle_events():
            self.assertNotEqual(event["distinct_id"], self.client._device_id)
            self.assertNotIn(event["distinct_id"], ("system", "anonymous"))
            self.assertFalse(event["distinct_id"].startswith("anon_"))

    def test_lifecycle_events_use_the_scope_user_when_identified(self):
        get_current_scope().set_user({"id": "u_42", "email": "a@b.co"})
        self.client.start_workflow("checkout")
        self.client.end_workflow()
        for event in self._lifecycle_events():
            self.assertEqual(event["distinct_id"], "u_42")

    def test_ordinary_events_still_require_a_distinct_id(self):
        """The exemption is scoped to the three reserved names only — a
        plain event with a falsy distinct_id is still dropped."""
        self.client.track("", "u_1", {})          # sanity: named event, real id
        self.client.track("ordinary_event", "", {})
        self.client.flush()
        names = [i["name"] for i in self.sender.items if i["type"] == "event"]
        self.assertNotIn("ordinary_event", names)

    def test_lifecycle_events_are_not_dropped_by_the_distinct_id_guard(self):
        """The whole point of the exemption: without it, `track()` would
        drop all three lifecycle events for an anonymous run."""
        self.client.start_workflow("checkout")
        self.client.cancel_workflow()
        self.assertEqual(len(self._lifecycle_events()), 2)


class WorkflowClientDisabledGatingTests(unittest.TestCase):
    """`disabled` must be gated on "an *enabled* client", not just "a client
    object exists" — `Client.close()` flips `enabled = False` but the object
    itself stays reachable, so a bare null-check would miss this."""

    def setUp(self):
        reset_scopes()
        self.sender = FakeSender(status=200)
        self.client = Client(
            DSN, flush_interval=3600, max_batch=1000, sender=self.sender
        )

    def tearDown(self):
        reset_scopes()

    def test_start_after_close_is_disabled_and_does_not_mutate_state(self):
        first = self.client.start_workflow("checkout")
        self.assertEqual(first.status, sauron.WorkflowStatus.OK)

        self.client.close(timeout=2)
        self.assertFalse(self.client.enabled)

        second = self.client.start_workflow("other")
        self.assertEqual(second.status, sauron.WorkflowStatus.DISABLED)
        # State from before close() is untouched by the rejected call.
        wf = get_current_scope().workflow
        assert wf is not None
        self.assertEqual(wf.name, "checkout")

    def test_end_and_cancel_after_close_are_disabled(self):
        self.client.start_workflow("checkout")
        self.client.close(timeout=2)
        self.assertEqual(
            self.client.end_workflow().status, sauron.WorkflowStatus.DISABLED
        )
        self.assertEqual(
            self.client.cancel_workflow().status, sauron.WorkflowStatus.DISABLED
        )


class WorkflowClientAutoDisableTests(unittest.TestCase):
    """`Client.enabled` must also go `False` when the *transport* disables
    itself on a hard auth failure (401/403) — not just on an explicit
    `close()`, which ``WorkflowClientDisabledGatingTests`` already covers.

    A revoked/rotated DSN key must stop `start_workflow`/`track`/etc. from
    believing they succeeded: otherwise `start_workflow` keeps minting real
    workflow ids and stamping state the server will never see, and
    `get_workflow()` reports a workflow that was, from the server's
    perspective, never started. Nobody calls `close()` in either test below;
    the disable is driven through the transport's real `401`-handling path
    (`Transport._send` -> `Transport.disable`), the same path a live key
    revocation takes in production.
    """

    def setUp(self):
        reset_scopes()

    def tearDown(self):
        reset_scopes()

    def test_enabled_flips_false_after_a_real_401_with_no_close_call(self):
        """End-to-end smoke test: a real 401, synchronously flushed, leaves
        `enabled` `False` afterwards and blocks a subsequent `start_workflow`.
        """
        sender = FakeSender(status=401)
        client = Client(
            DSN, flush_interval=3600, max_batch=1000, sender=sender
        )
        try:
            self.assertTrue(client.enabled)

            # Enqueue then force a synchronous flush, exactly like a live
            # 401 rejection during ordinary traffic — no `close()` involved.
            client.track("ev", "u_1", {})
            client.flush()

            # Sanity: the send actually happened and was rejected.
            self.assertTrue(sender.calls)
            self.assertEqual(sender.calls[0]["headers"].get(
                "Content-Type"), "application/json")

            self.assertFalse(client.enabled)

            # The workflow guards must observe the same auto-disable.
            result = client.start_workflow("checkout")
            self.assertEqual(result.status, sauron.WorkflowStatus.DISABLED)
            self.assertIsNone(get_current_scope().workflow)
        finally:
            client.close(timeout=2)

    def test_enabled_reflects_the_transport_mid_disable_not_just_after(self):
        """The precise defect this guards against: `Transport.disable()`
        flips ``self._disabled = True`` *before* it invokes the client's
        ``on_disable`` callback (see ``_transport.py``). A `Client.enabled`
        that is only ever written *by* that callback (rather than reading
        the transport's own flag) has a real window, mid-disable, where the
        transport already knows it is dead but the client still reports
        ``enabled=True`` — exactly long enough for a concurrently-running
        `start_workflow` to slip through and mint state the server will
        never materialize.

        This test freezes that window open with a stalling stand-in for the
        callback and asserts `enabled` (and `start_workflow`) already see the
        disable *before* the callback finishes — i.e. the predicate must be
        computed from the transport's own flag, not from a side-effect the
        callback performs after the fact.
        """
        entered = threading.Event()
        release = threading.Event()

        sender = FakeSender(status=401)
        client = Client(
            DSN, flush_interval=3600, max_batch=1000, sender=sender
        )
        try:
            # Stall the transport's on_disable callback right at its start,
            # holding it open after Transport.disable() has already flipped
            # its own ``_disabled`` flag (that assignment always happens
            # before the callback runs; see ``Transport.disable``).
            original_on_disable = client._transport._on_disable

            def _stalling_on_disable() -> None:
                entered.set()
                release.wait(2)
                original_on_disable()

            client._transport._on_disable = _stalling_on_disable

            def _trigger() -> None:
                client.track("ev", "u_1", {})
                client.flush()

            sender_thread = threading.Thread(target=_trigger)
            sender_thread.start()
            try:
                self.assertTrue(
                    entered.wait(2), "on_disable callback never started"
                )

                # The transport already knows; the client-level predicate
                # must say so too, without waiting on the stalled callback.
                self.assertTrue(client._transport.disabled)
                self.assertFalse(client.enabled)

                result = client.start_workflow("checkout")
                self.assertEqual(
                    result.status, sauron.WorkflowStatus.DISABLED
                )
            finally:
                release.set()
                sender_thread.join(2)
        finally:
            client.close(timeout=2)


class WorkflowConcurrencyTests(unittest.TestCase):
    """The point of this task: workflow state must not be a module global."""

    def setUp(self):
        reset_scopes()
        self.sender = FakeSender(status=200)
        sauron.init(DSN, flush_interval=3600, max_batch=1000, sender=self.sender)

    def tearDown(self):
        sauron.close(timeout=2)
        reset_scopes()

    def test_workflow_does_not_leak_across_concurrent_asyncio_tasks(self):
        results = {}

        async def simulated_request(worker: str, workflow_name: str, delay: float):
            # Each "request" pushes its own scope, exactly like the request
            # handler pattern documented in the README/example — this is what
            # makes the ContextVar-backed isolation apply per request rather
            # than against the shared global scope.
            with sauron.scope():
                start = sauron.start_workflow(workflow_name)
                results[f"{worker}_start_status"] = start.status
                results[f"{worker}_start_id"] = start.workflow_id

                # Yield control to the event loop here. If the active
                # workflow were a bare module-level global (as in the
                # browser reference SDK), the OTHER task's start_workflow —
                # running while this task is suspended below — would
                # overwrite it, and everything after this line would
                # observe the wrong workflow (or none).
                await asyncio.sleep(delay)

                sauron.track(f"{worker}_event", "u_1", {})
                try:
                    raise ValueError(f"{worker} failure")
                except ValueError:
                    sauron.capture_exception()

                wf = sauron.get_workflow()
                results[f"{worker}_wf_name_after_sleep"] = wf.name if wf else None
                results[f"{worker}_wf_id_after_sleep"] = (
                    wf.workflow_id if wf else None
                )

        async def run_both():
            # `asyncio.gather` schedules each coroutine as its own Task, and
            # each Task copies the *current* contextvars Context at creation
            # time; the `push_scope()` a task performs inside itself then
            # only rebinds its own copy. Both tasks share ONE OS thread.
            await asyncio.gather(
                simulated_request("a", "workflow-a", delay=0.05),
                simulated_request("b", "workflow-b", delay=0.0),
            )

        asyncio.run(run_both())

        # Each task's own start succeeded with its own fresh id.
        self.assertEqual(results["a_start_status"], sauron.WorkflowStatus.OK)
        self.assertEqual(results["b_start_status"], sauron.WorkflowStatus.OK)
        self.assertNotEqual(results["a_start_id"], results["b_start_id"])

        # After the interleaved sleep, each task still sees ONLY its own
        # workflow — this assertion is what a module-level global would fail:
        # task "b" runs start_workflow("workflow-b") while task "a" is
        # suspended inside its own `await asyncio.sleep`, so a shared global
        # would have "a" wake up and observe "workflow-b" (or, once "b" also
        # finishes and clears/replaces state, `None` or a third value).
        self.assertEqual(results["a_wf_name_after_sleep"], "workflow-a")
        self.assertEqual(results["b_wf_name_after_sleep"], "workflow-b")
        self.assertEqual(results["a_wf_id_after_sleep"], results["a_start_id"])
        self.assertEqual(results["b_wf_id_after_sleep"], results["b_start_id"])

        # Cross-check on the wire: each task's tracked event and captured
        # error carry its OWN workflow id/name, never the sibling's.
        sauron.flush()
        by_name = {}
        for item in self.sender.items:
            if item["type"] == "event" and item["name"] in ("a_event", "b_event"):
                by_name[item["name"]] = item
        errors_by_value = {
            i["exception"]["value"]: i
            for i in self.sender.items
            if i["type"] == "error"
        }

        self.assertEqual(by_name["a_event"]["workflow_id"], results["a_start_id"])
        self.assertEqual(by_name["a_event"]["workflow_name"], "workflow-a")
        self.assertEqual(by_name["b_event"]["workflow_id"], results["b_start_id"])
        self.assertEqual(by_name["b_event"]["workflow_name"], "workflow-b")

        self.assertEqual(
            errors_by_value["a failure"]["workflow_id"], results["a_start_id"]
        )
        self.assertEqual(
            errors_by_value["a failure"]["workflow_name"], "workflow-a"
        )
        self.assertEqual(
            errors_by_value["b failure"]["workflow_id"], results["b_start_id"]
        )
        self.assertEqual(
            errors_by_value["b failure"]["workflow_name"], "workflow-b"
        )

    def test_workflow_does_not_leak_across_concurrent_threads(self):
        """Same guarantee, exercised the way `_scope.py`'s own isolation
        test (`test_isolation_across_copied_contexts`) already does it —
        real OS threads. A plain module-level global is genuinely shared
        process-wide across threads (unlike a `ContextVar`, which defaults
        independently per thread), so this is a second, structurally
        different proof of the same property.
        """
        import threading

        barrier = threading.Barrier(2)
        results = {}

        def worker(name: str, workflow_name: str):
            with sauron.scope():
                start = sauron.start_workflow(workflow_name)
                results[f"{name}_status"] = start.status
                results[f"{name}_id"] = start.workflow_id
                barrier.wait()  # force both threads to have started before either tracks
                sauron.track(f"{name}_thread_event", "u_1", {})
                wf = sauron.get_workflow()
                results[f"{name}_wf_name"] = wf.name if wf else None

        t1 = threading.Thread(target=worker, args=("t1", "wf-thread-1"))
        t2 = threading.Thread(target=worker, args=("t2", "wf-thread-2"))
        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        self.assertEqual(results["t1_status"], sauron.WorkflowStatus.OK)
        self.assertEqual(results["t2_status"], sauron.WorkflowStatus.OK)
        self.assertNotEqual(results["t1_id"], results["t2_id"])
        self.assertEqual(results["t1_wf_name"], "wf-thread-1")
        self.assertEqual(results["t2_wf_name"], "wf-thread-2")

        sauron.flush()
        by_name = {
            i["name"]: i
            for i in self.sender.items
            if i["type"] == "event" and i["name"].endswith("_thread_event")
        }
        self.assertEqual(by_name["t1_thread_event"]["workflow_id"], results["t1_id"])
        self.assertEqual(by_name["t2_thread_event"]["workflow_id"], results["t2_id"])


if __name__ == "__main__":
    unittest.main()
