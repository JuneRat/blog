import json
from pathlib import Path
import unittest

import recovery_inventory as inventory


class MediaUrlTests(unittest.TestCase):
    def test_shared_browser_and_rust_normalization_contract(self):
        path = Path(__file__).resolve().parent.parent / "crates/infrastructure/tests/fixtures/media_url_normalization.json"
        matrix = json.loads(path.read_text())
        for case in matrix["cases"]:
            with self.subTest(case=case["name"]):
                self.assertEqual(inventory.media_url_id(case["source"]), case["expected_id"])
        for version in (0, 1):
            for source in (f'/media/./{matrix["media_id"]}', f'/media/%2e/{matrix["media_id"]}',
                           f'\\media\\{matrix["media_id"]}', f' /media/{matrix["media_id"]}'):
                self.assertIsNone(inventory.media_url_id(source, version))

    def relations(self, pipeline_version, actual):
        canonical = "12345678-1234-5678-9abc-123456789abc"
        query = "12345678-1234-5678-9abc-123456789abd"
        post = "12345678-1234-5678-9abc-123456789abe"

        class Pg:
            def query(self, sql, database):
                if "FROM posts" in sql:
                    assert "content_render_version" in sql
                    return json.dumps([{
                        "id": post, "cover": None, "pipeline_version": pipeline_version,
                        "html": f'<img src="/media/{canonical}"><img src="/media/{query}?v=1">',
                    }])
                if "FROM media_refs" in sql:
                    return json.dumps([[media_id, "post", post] for media_id in actual])
                if "WITH RECURSIVE" in sql:
                    return "0"
                return "[]"

        return inventory.validate_relations(Pg())

    def test_legacy_backup_relations_use_the_saved_extraction_contract(self):
        canonical = "12345678-1234-5678-9abc-123456789abc"
        for version in (0, 1):
            with self.subTest(version=version):
                self.assertEqual(self.relations(version, [canonical]), 1)
                self.assertIsNone(inventory.media_url_id(f"/media/{canonical}?v=1", version))

    def test_current_pipeline_rejects_missing_query_image_bookkeeping(self):
        canonical = "12345678-1234-5678-9abc-123456789abc"
        query = "12345678-1234-5678-9abc-123456789abd"
        with self.assertRaisesRegex(inventory.RecoveryError, "1 missing"):
            self.relations(2, [canonical])
        self.assertEqual(self.relations(2, [canonical, query]), 2)

    def test_unknown_pipeline_versions_cannot_be_silently_verified(self):
        for version in (-1, 3, 100):
            with self.subTest(version=version):
                with self.assertRaisesRegex(inventory.RecoveryError, "unsupported content pipeline"):
                    self.relations(version, [])

    def test_local_url_identity_matches_http_path_decoding(self):
        media_id = "12345678-1234-5678-9abc-123456789abc"
        encoded = "".join(f"%{ord(char):02x}" for char in media_id)
        for target in (
            f"/media/{media_id}?v=1&size=large",
            f"/media/{media_id}#preview",
            f"/media/{media_id}?v=1#preview",
            f"/media/{encoded}?v=1",
            f"/media/%7B{media_id}%7D",
            f"/media/{media_id.replace('-', '')}",
            f"/media/urn:uuid:{media_id}",
        ):
            with self.subTest(target=target):
                self.assertEqual(inventory.media_url_id(target), media_id)
                parser = inventory.Images()
                parser.feed(f'<img src="{target}">')
                self.assertEqual(parser.ids, {media_id})

    def test_external_and_unaccepted_paths_do_not_claim_local_media(self):
        media_id = "12345678-1234-5678-9abc-123456789abc"
        for target in (
            f"https://example.com/media/{media_id}",
            f"//example.com/media/{media_id}",
            f"/media/{media_id}/extra",
            f"/media/%2524{media_id}",
            f"/media/{media_id}%2F",
            f"/media/%FF{media_id}",
            f"/%6Dedia/{media_id}",
            "/media/----12345678123456789abc123456789abc",
        ):
            with self.subTest(target=target):
                self.assertIsNone(inventory.media_url_id(target))
                parser = inventory.Images()
                parser.feed(f'<img src="{target}">')
                self.assertEqual(parser.ids, set())


    def test_theme_references_use_persisted_declared_media_fields(self):
        mid = "12345678-1234-5678-9abc-123456789abc"
        tid = "12345678-1234-5678-9abc-123456789abe"
        class Pg:
            actual = [[mid, "theme", tid]]
            config = {"image": mid, "text": mid}
            fields = ["image"]
            def query(self, sql, database):
                if "FROM themes" in sql:
                    return json.dumps([{"id": tid, "config": self.config, "media_fields": self.fields}])
                if "FROM media_refs" in sql:
                    return json.dumps(self.actual)
                if "WITH RECURSIVE" in sql:
                    return "0"
                return "[]"
        pg = Pg()
        self.assertEqual(inventory.validate_relations(pg), 1)
        pg.actual = []
        with self.assertRaisesRegex(inventory.RecoveryError, "1 missing"):
            inventory.validate_relations(pg)
        pg.actual = [[mid, "theme", tid]]
        pg.config = {"image": "invalid"}
        with self.assertRaisesRegex(inventory.RecoveryError, "invalid theme media ID"):
            inventory.validate_relations(pg)
        pg.fields = ["image", "image"]
        with self.assertRaisesRegex(inventory.RecoveryError, "invalid theme media field inventory"):
            inventory.validate_relations(pg)


if __name__ == "__main__":
    unittest.main()
