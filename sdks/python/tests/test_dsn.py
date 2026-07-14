import unittest

from sauron._dsn import DsnError, parse_dsn


class TestDsn(unittest.TestCase):
    def test_parses_valid_dsn_with_port(self):
        dsn = parse_dsn("https://pk_test@localhost:8081/1")
        self.assertEqual(dsn.public_key, "pk_test")
        self.assertEqual(dsn.host, "localhost:8081")
        self.assertEqual(dsn.hostname, "localhost")
        self.assertEqual(dsn.protocol, "https")
        self.assertEqual(dsn.project_id, "1")
        self.assertEqual(
            dsn.envelope_url, "https://localhost:8081/api/1/envelope"
        )
        self.assertEqual(dsn.raw, "https://pk_test@localhost:8081/1")

    def test_parses_valid_dsn_without_port(self):
        dsn = parse_dsn("http://pk@ingest.example.com/42")
        self.assertEqual(dsn.host, "ingest.example.com")
        self.assertEqual(dsn.protocol, "http")
        self.assertEqual(dsn.project_id, "42")
        self.assertEqual(
            dsn.envelope_url, "http://ingest.example.com/api/42/envelope"
        )

    def test_empty_dsn_raises(self):
        with self.assertRaises(DsnError):
            parse_dsn("")

    def test_missing_public_key_raises(self):
        with self.assertRaises(DsnError):
            parse_dsn("https://localhost:8081/1")

    def test_password_component_raises(self):
        with self.assertRaises(DsnError):
            parse_dsn("https://pk:secret@localhost:8081/1")

    def test_missing_project_id_raises(self):
        with self.assertRaises(DsnError):
            parse_dsn("https://pk@localhost:8081/")

    def test_unsupported_protocol_raises(self):
        with self.assertRaises(DsnError):
            parse_dsn("ftp://pk@localhost/1")

    def test_not_a_url_raises(self):
        with self.assertRaises(DsnError):
            parse_dsn("not-a-dsn")


if __name__ == "__main__":
    unittest.main()
