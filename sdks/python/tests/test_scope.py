import contextvars
import unittest

from sauron._scope import (
    Scope,
    configure_scope,
    get_current_scope,
    get_global_scope,
    pop_scope,
    push_scope,
    reset_scopes,
    scope,
)


class TestScopeUnit(unittest.TestCase):
    def tearDown(self):
        reset_scopes()

    def test_ring_buffer_caps_and_drops_oldest(self):
        s = Scope(max_breadcrumbs=3)
        for i in range(5):
            s.add_breadcrumb({"message": str(i)})
        self.assertEqual(
            [b["message"] for b in s.breadcrumbs], ["2", "3", "4"]
        )

    def test_clone_is_independent(self):
        parent = Scope()
        parent.set_tag("env", "prod")
        parent.add_breadcrumb({"message": "a"})
        child = parent.clone()
        child.set_tag("req", "42")
        child.add_breadcrumb({"message": "b"})
        # Mutating the child must not touch the parent.
        self.assertEqual(parent.tags, {"env": "prod"})
        self.assertEqual([b["message"] for b in parent.breadcrumbs], ["a"])
        self.assertEqual(child.tags, {"env": "prod", "req": "42"})
        self.assertEqual(
            [b["message"] for b in child.breadcrumbs], ["a", "b"]
        )

    def test_apply_to_error_merges_scope_state(self):
        s = Scope()
        s.set_tag("env", "prod")
        s.set_user({"id": "u_1", "email": "a@b.co"})
        s.add_breadcrumb({"message": "clicked"})
        s.set_context("order", {"id": 7})
        s.set_extra("k", "v")
        item = {"type": "error", "tags": {}}
        s.apply_to_error(item)
        self.assertEqual(item["tags"], {"env": "prod"})
        self.assertEqual(item["user"], {"id": "u_1", "email": "a@b.co"})
        self.assertEqual(
            [b["message"] for b in item["breadcrumbs"]], ["clicked"]
        )
        self.assertEqual(item["contexts"], {"order": {"id": 7}})
        self.assertEqual(item["extra"], {"k": "v"})

    def test_apply_to_error_per_call_values_win(self):
        s = Scope()
        s.set_tag("env", "prod")
        s.set_user({"id": "scoped"})
        item = {
            "type": "error",
            "tags": {"env": "override", "area": "billing"},
            "user": {"id": "explicit"},
        }
        s.apply_to_error(item)
        self.assertEqual(
            item["tags"], {"env": "override", "area": "billing"}
        )
        self.assertEqual(item["user"], {"id": "explicit"})

    def test_apply_to_error_omits_empty_optional_blocks(self):
        s = Scope()
        item = {"type": "error", "tags": {}}
        s.apply_to_error(item)
        self.assertNotIn("contexts", item)
        self.assertNotIn("extra", item)
        self.assertEqual(item["breadcrumbs"], [])
        self.assertEqual(item["tags"], {})


class TestScopeStack(unittest.TestCase):
    def tearDown(self):
        reset_scopes()

    def test_current_is_global_by_default(self):
        self.assertIs(get_current_scope(), get_global_scope())

    def test_global_and_child_tags_both_land(self):
        get_global_scope().set_tag("env", "prod")
        with scope() as s:
            s.set_tag("req", "42")
            item = {"type": "error", "tags": {}}
            get_current_scope().apply_to_error(item)
            self.assertEqual(item["tags"], {"env": "prod", "req": "42"})
        # After the block the child is popped; global unaffected by req.
        self.assertIs(get_current_scope(), get_global_scope())
        item2 = {"type": "error", "tags": {}}
        get_current_scope().apply_to_error(item2)
        self.assertEqual(item2["tags"], {"env": "prod"})

    def test_push_pop_restores_parent(self):
        get_global_scope().set_tag("env", "prod")
        child = push_scope()
        self.assertIsNot(child, get_global_scope())
        child.set_tag("req", "1")
        self.assertEqual(get_current_scope().tags, {"env": "prod", "req": "1"})
        pop_scope()
        self.assertIs(get_current_scope(), get_global_scope())

    def test_nested_scopes_restore_correctly(self):
        with scope() as outer:
            outer.set_tag("level", "outer")
            with scope() as inner:
                inner.set_tag("level", "inner")
                self.assertEqual(get_current_scope().tags["level"], "inner")
            self.assertEqual(get_current_scope().tags["level"], "outer")

    def test_configure_scope_mutates_current(self):
        configure_scope(lambda s: s.set_tag("release", "1.0"))
        self.assertEqual(get_current_scope().tags["release"], "1.0")

    def test_isolation_across_copied_contexts(self):
        results = {}

        def worker(name):
            with scope() as s:
                s.set_tag("id", name)
                item = {"type": "error", "tags": {}}
                get_current_scope().apply_to_error(item)
                results[name] = item["tags"].get("id")

        ctx_a = contextvars.copy_context()
        ctx_b = contextvars.copy_context()
        ctx_a.run(worker, "A")
        ctx_b.run(worker, "B")
        self.assertEqual(results, {"A": "A", "B": "B"})
        # Neither leaked into the outer/global scope.
        self.assertIs(get_current_scope(), get_global_scope())
        self.assertNotIn("id", get_global_scope().tags)


if __name__ == "__main__":
    unittest.main()
