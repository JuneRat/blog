"""Real draft/publication/history cycles for the isolated release load test."""
import json
import re
import uuid

from acceptance_support import API, require
from benchmark_public import MixedWriter, Samples, measure_write

REVISION_LIMIT = 50


class RevisionWriter(MixedWriter):
    """One writer owns the body; other clients may reorder its shared series."""

    def __init__(self, client, guest, post_id, content, series_slug=None):
        super().__init__(client, post_id, content, series_slug)
        self.guest = guest
        self.path = f"{API}/posts/{post_id}"
        self.marker_prefix = "CapacityRevision" + uuid.uuid4().hex
        self.live_marker = None
        self.completed_cycles = 0
        self.initial_version = client.json("GET", self.path)["version"]
        self.phases = {name: Samples() for name in
                       ("draft_save", "draft_visibility", "publish", "published_visibility", "history")}

    def phase(self, name, operation):
        return measure_write(operation, self.phases[name], raise_errors=True)

    def draft(self):
        current = self.client.json("GET", self.path)
        self.sequence += 1
        # A trailing delimiter prevents edit 1 matching edit 10.
        marker = f"{self.marker_prefix}Edit{self.sequence}End"
        content = self.content + "\n\n" + marker
        saved = self.client.json("PATCH", self.path, {
            "content": content, "expected_version": current["version"],
        })
        require(saved["id"] == self.post_id and saved["content"] == content
                and saved["version"] > current["version"] and saved["has_pending_changes"]
                and saved["status"] == "published", "draft save contract failed")
        require(re.fullmatch(r"capacity-(?:post|\d+)", saved["slug"]), "unexpected fixture slug")
        return saved, marker

    def visible(self, saved, marker, published):
        body = self.guest.request("GET", "/posts/" + saved["slug"])[0]
        require((marker.encode() in body) == published, "draft/publication visibility contract failed")
        if not published and self.live_marker:
            require(self.live_marker.encode() in body, "draft save changed the previous publication")

    def publish(self, saved, marker):
        result = self.client.json("POST", self.path + "/publish", {"expected_version": saved["version"]})
        require(result["id"] == self.post_id and result["content"] == saved["content"]
                and result["version"] > saved["version"] and not result["has_pending_changes"]
                and result["status"] == "published", "publish contract failed")
        self.live_marker = marker
        return result

    def history(self, published):
        rows = self.client.json("GET", self.path + "/revisions")
        versions = [row["version"] for row in rows]
        require(0 < len(rows) <= REVISION_LIMIT and versions == sorted(set(versions), reverse=True),
                "revision history order or bound failed")
        revision = next((row for row in rows if row["version"] == published["version"]), None)
        require(revision is not None, "published revision is missing")
        detail = self.client.json("GET", self.path + "/revisions/" + revision["id"])
        require(detail["content"] == published["content"], "revision body differs from published edit")

    def save(self):
        # benchmark_public.load measures this entire cycle as writes.save.
        # Phase timings below distinguish API work from visibility/history checks.
        saved, marker = self.phase("draft_save", self.draft)
        self.phase("draft_visibility", lambda: self.visible(saved, marker, False))
        published = self.phase("publish", lambda: self.publish(saved, marker))
        self.phase("published_visibility", lambda: self.visible(published, marker, True))
        self.phase("history", lambda: self.history(published))
        self.completed_cycles += 1

    def finish(self, query, minimum_cycles):
        """Outside timing: leave a pending edit and inspect actual stored history."""
        require(self.completed_cycles >= minimum_cycles, "writer completed too few full publication cycles")
        saved, marker = self.draft()
        self.visible(saved, marker, False)
        current = self.client.json("GET", self.path)
        require(current["has_pending_changes"] and current["content"] == saved["content"],
                "final editing draft was not retained")
        post_id = str(uuid.UUID(self.post_id))
        evidence = json.loads(query(
            "SELECT jsonb_build_object('retained_revisions',count(*),'oldest_version',min(r.version),"
            "'newest_version',max(r.version),'active_draft_retained',bool_or(r.id=p.draft_revision_id),"
            "'active_draft_differs_from_publication',bool_or(r.id=p.draft_revision_id AND r.data->>'content'<>p.content)) "
            f"FROM posts p JOIN content_revisions r ON r.post_id=p.id WHERE p.id='{post_id}'"))
        # API lists cap at 50 even if pruning is broken; inspect the physical rows.
        require(evidence["retained_revisions"] == REVISION_LIMIT
                and evidence["oldest_version"] > self.initial_version
                and evidence["newest_version"] == current["version"]
                and evidence["active_draft_retained"] and evidence["active_draft_differs_from_publication"],
                "stored revision pruning or pending draft integrity failed")
        return {"completed_cycles": self.completed_cycles, **evidence,
                "pending_draft_private": True, "original_revision_pruned": True}


def writing_summary(writers):
    phases = {name: Samples() for name in writers[0].phases}
    for writer in writers:
        for name, samples in writer.phases.items():
            phases[name].merge(samples)
    return {"mode": "draft_publish_history_cycle", "revision_limit": REVISION_LIMIT,
            "completed_cycles_per_writer": [writer.completed_cycles for writer in writers],
            "phases": {name: samples.summary() for name, samples in phases.items()}}
