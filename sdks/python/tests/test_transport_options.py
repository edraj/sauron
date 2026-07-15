"""The transport options (gzip / queue / offline) are wired through the public
``sauron.init`` surface, not just the low-level ``Transport``."""

import os
import unittest

import sauron
from sauron._scope import reset_scopes

from ._fake import FakeSender

DSN = "https://pk_test@localhost:8081/1"


class TestInitForwardsTransportOptions(unittest.TestCase):
    def setUp(self):
        reset_scopes()
        self.sender = FakeSender(status=200)

    def tearDown(self):
        sauron.close(timeout=2)
        reset_scopes()

    def test_gzip_threshold_is_honored(self):
        sauron.init(
            DSN,
            flush_interval=3600,
            max_batch=1000,
            gzip_threshold_bytes=64,
            sender=self.sender,
        )
        sauron.track("big", "u_1", properties={"blob": "x" * 4000})
        sauron.flush()
        # FakeSender transparently gunzips, so the payload is still readable and
        # the compression header rode along.
        self.assertEqual(
            self.sender.calls[0]["headers"].get("Content-Encoding"), "gzip"
        )
        self.assertEqual(
            self.sender.items[0]["properties"]["blob"], "x" * 4000
        )

    def test_offline_path_persists_pending_items(self):
        # A failing sender leaves the item on disk for a later process.
        with self.assertTempDir() as d:
            sauron.init(
                DSN,
                flush_interval=3600,
                max_batch=1000,
                offline_path=d,
                sender=FakeSender(status=503),
            )
            sauron.track("job", "u_1")
            self.assertEqual(len(os.listdir(d)), 1)

    # -- helpers ----------------------------------------------------------

    def assertTempDir(self):
        import tempfile

        class _Ctx:
            def __enter__(self):
                self.dir = tempfile.mkdtemp(prefix="sauron-offline-")
                return self.dir

            def __exit__(self, *exc):
                import shutil

                shutil.rmtree(self.dir, ignore_errors=True)
                return False

        return _Ctx()


if __name__ == "__main__":
    unittest.main()
