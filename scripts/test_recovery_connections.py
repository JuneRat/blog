"""Recovery connections must preserve explicit policies without changing targets."""
import os
import unittest
from unittest.mock import patch

import recovery


class RecoveryConnectionTests(unittest.TestCase):
    def test_native_tools_preserve_tls_ca_and_timeout_when_switching_databases(self):
        url = ("postgres://owner:p%40ss@db.example/blog?sslmode=verify-full"
               "&sslrootcert=%2Fprivate%2Froot%20ca.pem&sslcert=%2Fprivate%2Fclient.pem"
               "&sslkey=%2Fprivate%2Fkey.pem&ssl_min_protocol_version=TLSv1.3"
               "&channel_binding=require&connect_timeout=3"
               "&options=-cstatement_timeout%3D2000&application_name=restore-check")
        with patch.dict(os.environ, {"PGSSLMODE": "disable", "PGDATABASE": "wrong"}, clear=True):
            pg = recovery.PgTools(url)
            for tool in ("psql", "pg_dump", "pg_restore", "createdb"):
                command, env = pg.command(tool, ["--version"], "blog_restore_unit")
                self.assertEqual(command, [tool, "--version"])
                self.assertEqual(env["PGHOST"], "db.example")
                self.assertEqual(env["PGUSER"], "owner")
                self.assertEqual(env["PGPASSWORD"], "p@ss")
                self.assertEqual(env["PGDATABASE"], "blog_restore_unit")
                self.assertEqual(env["PGSSLMODE"], "verify-full")
                self.assertEqual(env["PGSSLROOTCERT"], "/private/root ca.pem")
                self.assertEqual(env["PGSSLCERT"], "/private/client.pem")
                self.assertEqual(env["PGSSLKEY"], "/private/key.pem")
                self.assertEqual(env["PGSSLMINPROTOCOLVERSION"], "TLSv1.3")
                self.assertEqual(env["PGCHANNELBINDING"], "require")
                self.assertEqual(env["PGCONNECT_TIMEOUT"], "3")
                self.assertEqual(env["PGOPTIONS"], "-cstatement_timeout=2000")
                self.assertEqual(env["PGAPPNAME"], "restore-check")
                self.assertNotIn("p@ss", " ".join(command))

    def test_ambiguous_or_unhandled_policies_fail_before_a_command_is_built(self):
        for suffix in ("sslmode=", "sslmode=require&sslmode=disable", "sslmode",
                       "sslmode=verify-full&unknown=secret", "dbname=other",
                       "host=other", "sslrootcert=%00", "sslmode=require#ignored"):
            with self.subTest(suffix=suffix):
                with self.assertRaises((recovery.RecoveryError, ValueError)):
                    recovery.PgTools("postgres://owner@localhost/blog?" + suffix)

    def test_docker_socket_mode_rejects_policies_it_cannot_honor(self):
        with self.assertRaisesRegex(recovery.RecoveryError, "Docker mode"):
            recovery.PgTools("postgres://owner@localhost/blog?sslmode=verify-full", "blog-postgres")
        command, _ = recovery.PgTools("postgres://owner@localhost/blog", "blog-postgres").command(
            "psql", ["--version"], "blog_restore_unit")
        self.assertIn("PGDATABASE=blog_restore_unit", command)


if __name__ == "__main__":
    unittest.main()
