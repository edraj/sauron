"""Metadata scopes (tags / contexts / extra) across init, per-capture, and track."""

import unittest

import sauron
from sauron._client import Client
from sauron._scope import get_current_scope, get_global_scope, reset_scopes

from ._fake import FakeSender

DSN = "https://pk_test@localhost:8081/1"


class TestInitDefaults(unittest.TestCase):
    """init()/Client() tags/contexts/extra seed the global scope."""

    def setUp(self):
        reset_scopes()

    def tearDown(self):
        sauron.close()
        reset_scopes()

    def test_init_seeds_global_scope(self):
        sauron.init(
            DSN,
            flush_interval=3600,
            max_batch=1000,
            tags={"service": "api"},
            contexts={"order": {"id": 7}},
            extra={"build": "abc123"},
            sender=FakeSender(status=200),
        )
        g = get_global_scope()
        self.assertEqual(g.tags, {"service": "api"})
        self.assertEqual(g.contexts, {"order": {"id": 7}})
        self.assertEqual(g.extra, {"build": "abc123"})

    def test_init_defaults_flow_onto_captured_error(self):
        fake = FakeSender(status=200)
        sauron.init(
            DSN,
            flush_interval=3600,
            max_batch=1000,
            tags={"service": "api"},
            contexts={"order": {"id": 7}},
            extra={"build": "abc123"},
            sender=fake,
        )
        try:
            raise ValueError("boom")
        except ValueError as exc:
            sauron.capture_exception(exc)
        sauron.flush()
        err = fake.items[0]
        self.assertEqual(err["tags"], {"service": "api"})
        self.assertEqual(err["contexts"], {"order": {"id": 7}})
        self.assertEqual(err["extra"], {"build": "abc123"})


class TestPerCaptureMetadata(unittest.TestCase):
    def setUp(self):
        reset_scopes()
        self.fake = FakeSender(status=200)
        self.client = Client(
            DSN, flush_interval=3600, max_batch=1000, sender=self.fake
        )

    def tearDown(self):
        self.client.close(timeout=2)
        reset_scopes()

    def test_capture_exception_per_call_contexts_extra_override_scope(self):
        get_current_scope().set_context("order", {"id": 1})
        get_current_scope().set_extra("a", "scope")
        try:
            raise ValueError("boom")
        except ValueError as exc:
            self.client.capture_exception(
                exc,
                tags={"area": "billing"},
                contexts={"order": {"id": 99}, "cart": {"n": 2}},
                extra={"a": "call", "b": "call"},
            )
        self.client.flush()
        err = self.fake.items[0]
        self.assertEqual(err["tags"], {"area": "billing"})
        # contexts merge by block name (per-call "order" replaces scope "order").
        self.assertEqual(err["contexts"], {"order": {"id": 99}, "cart": {"n": 2}})
        # extra merges by shallow key (per-call "a" wins).
        self.assertEqual(err["extra"], {"a": "call", "b": "call"})

    def test_capture_message_attaches_scope_and_per_call(self):
        get_current_scope().set_tag("env", "prod")
        get_current_scope().set_context("order", {"id": 7})
        self.client.capture_message(
            "hi",
            tags={"area": "auth"},
            contexts={"cart": {"n": 2}},
            extra={"k": "v"},
        )
        self.client.flush()
        msg = self.fake.items[0]
        self.assertEqual(msg["tags"], {"env": "prod", "area": "auth"})
        self.assertEqual(msg["contexts"], {"order": {"id": 7}, "cart": {"n": 2}})
        self.assertEqual(msg["extra"], {"k": "v"})

    def test_capture_message_omits_empty_metadata(self):
        self.client.capture_message("hi")
        self.client.flush()
        msg = self.fake.items[0]
        self.assertEqual(msg["tags"], {})
        self.assertNotIn("contexts", msg)
        self.assertNotIn("extra", msg)


class TestTrackMetadata(unittest.TestCase):
    def setUp(self):
        reset_scopes()
        self.fake = FakeSender(status=200)
        self.client = Client(
            DSN, flush_interval=3600, max_batch=1000, sender=self.fake
        )

    def tearDown(self):
        self.client.close(timeout=2)
        reset_scopes()

    def _event(self):
        self.client.flush()
        return self.fake.items[0]

    def test_track_attaches_scope_metadata(self):
        get_current_scope().set_tag("env", "prod")
        get_current_scope().set_context("order", {"id": 7})
        get_current_scope().set_extra("build", "abc")
        self.client.track("checkout", "u_1", {"v": 1})
        ev = self._event()
        self.assertEqual(ev["tags"], {"env": "prod"})
        self.assertEqual(ev["contexts"], {"order": {"id": 7}})
        self.assertEqual(ev["extra"], {"build": "abc"})

    def test_track_per_call_overrides_scope_per_key(self):
        get_current_scope().set_tag("env", "prod")
        get_current_scope().set_context("order", {"id": 1})
        get_current_scope().set_extra("a", "scope")
        self.client.track(
            "checkout",
            "u_1",
            tags={"env": "staging", "area": "billing"},
            contexts={"order": {"id": 99}, "cart": {"n": 2}},
            extra={"a": "call", "b": "call"},
        )
        ev = self._event()
        self.assertEqual(ev["tags"], {"env": "staging", "area": "billing"})
        self.assertEqual(ev["contexts"], {"order": {"id": 99}, "cart": {"n": 2}})
        self.assertEqual(ev["extra"], {"a": "call", "b": "call"})

    def test_track_omits_empty_metadata(self):
        self.client.track("ping", "u_1")
        ev = self._event()
        self.assertNotIn("tags", ev)
        self.assertNotIn("contexts", ev)
        self.assertNotIn("extra", ev)
