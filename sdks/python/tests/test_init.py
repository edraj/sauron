import unittest

import sauron


class InitReleaseTests(unittest.TestCase):
    def tearDown(self):
        sauron.init(None)  # back to disabled

    def test_missing_release_raises(self):
        with self.assertRaises(ValueError):
            sauron.init("https://pk_test@localhost:8081/1")

    def test_blank_release_raises(self):
        with self.assertRaises(ValueError):
            sauron.init("https://pk_test@localhost:8081/1", release="  ")

    def test_release_is_validated_before_the_dsn_is_parsed(self):
        # Documented in `init`'s docstring: a call that gets BOTH wrong reports
        # the release, not the DSN. Pinned here because the order is an
        # accident of statement order in `init` and reads as a DsnError bug
        # report if it ever flips.
        #
        # `DsnError` SUBCLASSES `ValueError`, so `assertRaises(ValueError)`
        # alone would pass either way — the ordering claim only bites if we
        # also assert the raised exception is *not* a DsnError.
        with self.assertRaises(ValueError) as ctx:
            sauron.init("not-a-dsn")
        self.assertNotIsInstance(ctx.exception, sauron.DsnError)
        with self.assertRaises(sauron.DsnError):
            sauron.init("not-a-dsn", release="1.0.0")

    def test_empty_dsn_still_disables_without_release(self):
        self.assertIsNone(sauron.init(""))

    def test_release_is_trimmed(self):
        client = sauron.init("https://pk_test@localhost:8081/1", release=" 1.0.0 ")
        # `init` is typed `Optional[Client]` (a blank DSN disables the SDK and
        # returns None), so assert it is a Client before reading `.release` —
        # otherwise a regression that disabled this call would surface as an
        # AttributeError on None rather than as this assertion. A bare
        # `assert` (not `assertIsNotNone`) so type checkers narrow away the
        # Optional for the `client.release` read below.
        assert client is not None
        self.assertEqual(client.release, "1.0.0")
