"""A benchmark must fail on leaked drafts, partial cycles and hidden history growth."""
import json
import unittest
from unittest.mock import Mock

from acceptance_support import API, AcceptanceError
from benchmark_public import Samples, measure_write
from benchmark_writing import RevisionWriter, writing_summary

POST_ID = "00000000-0000-0000-0000-000000000001"
PATH = f"{API}/posts/{POST_ID}"


class WritingAPI:
    def __init__(self):
        self.version, self.live, self.draft = 1, "Body", None
        self.revisions = [{"id": "r1", "version": 1, "content": "Body"}]
        self.leak, self.conflict, self.corrupt_history = False, False, False
        self.bodies = []

    def record(self, content):
        self.version += 1
        self.revisions.insert(0, {"id": f"r{self.version}", "version": self.version, "content": content})
        self.revisions = self.revisions[:50]

    def detail(self):
        return {"id": POST_ID, "slug": "capacity-post", "version": self.version,
                "content": self.draft or self.live, "has_pending_changes": self.draft is not None,
                "status": "published"}

    def json(self, method, path, body=None):
        if (method, path) == ("GET", PATH):
            return self.detail()
        if method == "PATCH" and path == PATH:
            assert body["expected_version"] == self.version
            self.draft = body["content"]
            self.record(self.draft)
            return self.detail()
        if (method, path) == ("POST", PATH + "/publish"):
            if self.conflict:
                raise AcceptanceError("POST /publish: HTTP 409 (version_conflict), expected 200")
            assert body["expected_version"] == self.version
            self.live, self.draft = self.draft, None
            self.record(self.live)
            return self.detail()
        if (method, path) == ("GET", PATH + "/revisions"):
            return self.revisions
        if method == "GET" and path.startswith(PATH + "/revisions/"):
            if self.corrupt_history:
                return {"content": "wrong"}
            return next(row for row in self.revisions if row["id"] == path.rsplit("/", 1)[1])
        raise AssertionError("unexpected writing request")

    def request(self, method, path):
        assert (method, path) == ("GET", "/posts/capacity-post")
        body = (self.draft if self.leak and self.draft else self.live).encode()
        self.bodies.append(body)
        return body, {}


class RevisionWriterTests(unittest.TestCase):
    def writer(self):
        api = WritingAPI()
        return api, RevisionWriter(api, api, POST_ID, "Body")

    def test_full_cycles_check_publication_and_bounded_history(self):
        api, writer = self.writer()
        for _ in range(30):
            writer.save()
        self.assertEqual(writer.completed_cycles, 30)
        self.assertEqual(len(api.revisions), 50)
        self.assertEqual(api.bodies[0], b"Body")
        self.assertNotEqual(api.bodies[0], api.bodies[1])
        summary = writing_summary([writer])
        self.assertEqual(summary["completed_cycles_per_writer"], [30])
        self.assertTrue(all(phase["requests"] == 30 and phase["errors"] == 0 for phase in summary["phases"].values()))

    def test_leaked_draft_stops_before_publication_and_counts_a_contract_error(self):
        api, writer = self.writer()
        api.leak = True
        cycles = Samples()
        measure_write(writer.save, cycles)
        self.assertEqual(writer.completed_cycles, 0)
        self.assertEqual(api.live, "Body")
        self.assertEqual(cycles.summary()["error_types"], {"contract_validation": 1})
        self.assertEqual(writer.phases["draft_visibility"].summary()["errors"], 1)
        self.assertEqual(writer.phases["publish"].summary()["requests"], 0)

    def test_publish_conflict_preserves_successful_save_but_not_a_completed_cycle(self):
        api, writer = self.writer()
        api.conflict = True
        cycles = Samples()
        measure_write(writer.save, cycles)
        self.assertEqual(writer.completed_cycles, 0)
        self.assertEqual(cycles.summary()["conflicts"], 1)
        self.assertEqual(writer.phases["draft_save"].summary()["status_counts"], {"200": 1})
        self.assertEqual(writer.phases["publish"].summary()["conflicts"], 1)

    def test_history_corruption_cannot_count_as_a_successful_cycle(self):
        api, writer = self.writer()
        api.corrupt_history = True
        cycles = Samples()
        measure_write(writer.save, cycles)
        self.assertEqual(writer.completed_cycles, 0)
        self.assertEqual(cycles.summary()["errors"], 1)
        self.assertEqual(writer.phases["history"].summary()["errors"], 1)

    def test_physical_pruning_and_active_draft_are_checked_beyond_the_api_list_limit(self):
        for count, retained, different in ((50, True, True), (51, True, True), (50, False, True), (50, True, False)):
            with self.subTest(count=count, retained=retained, different=different):
                api, writer = self.writer()
                for _ in range(30):
                    writer.save()
                query = Mock(side_effect=lambda _: json.dumps({
                    "retained_revisions": count, "oldest_version": 13, "newest_version": api.version,
                    "active_draft_retained": retained, "active_draft_differs_from_publication": different,
                }))
                if count == 50 and retained and different:
                    result = writer.finish(query, 30)
                    self.assertTrue(result["pending_draft_private"])
                    self.assertIsNotNone(api.draft)
                else:
                    with self.assertRaisesRegex(AcceptanceError, "stored revision"):
                        writer.finish(query, 30)
                self.assertIn("count(*)", query.call_args.args[0])

    def test_insufficient_cycles_cannot_claim_pruning_coverage(self):
        _, writer = self.writer()
        query = Mock()
        with self.assertRaisesRegex(AcceptanceError, "too few"):
            writer.finish(query, 30)
        query.assert_not_called()


if __name__ == "__main__":
    unittest.main()
