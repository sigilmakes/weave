from __future__ import annotations

import asyncio
import os
import sys
import unittest

SRC = os.path.join(os.path.dirname(__file__), "..", "src")
if SRC not in sys.path:
    sys.path.insert(0, SRC)

from rlm.history import Entry, HistoryEntries, HistoryNamespace, IndexRow  # noqa: E402


class Recorder:
    def __init__(self, replies):
        self.replies = replies
        self.calls = []

    async def __call__(self, kind, payload):
        self.calls.append((kind, payload))
        return self.replies[kind]


def history_namespace(replies):
    recorder = Recorder(replies)
    return HistoryNamespace("session-a", transport=recorder), recorder


ENTRY = {"id": "a1b2c3d4", "turn": 3, "role": "user", "tokens": 40, "text": "hello"}


class HistoryNamespaceTest(unittest.TestCase):
    def test_one_selector_encodes_position_id_and_region(self):
        history, recorder = history_namespace(
            {"history.get": {"entries": [ENTRY], "total": 1, "shown": 1}}
        )

        asyncio.run(history.get(-1))
        asyncio.run(history.get("a1b2c3d4"))
        asyncio.run(history.get(slice(12, 41)))
        asyncio.run(history.get(slice(-20, None)))
        asyncio.run(history.get(slice(None, 40)))
        selectors = [payload["selector"] for _, payload in recorder.calls]
        self.assertEqual(selectors, [
            {"kind": "position", "value": -1},
            {"kind": "id", "value": "a1b2c3d4"},
            {"kind": "range", "start": 12, "end": 41},
            {"kind": "range", "start": -20, "end": None},
            {"kind": "range", "start": None, "end": 40},
        ])
        self.assertTrue(all(kind == "history.get" for kind, _ in recorder.calls))

    def test_get_returns_a_list_and_reports_the_remainder(self):
        history, _ = history_namespace(
            {"history.get": {"entries": [ENTRY] * 10, "total": 45, "shown": 10}}
        )
        got = asyncio.run(history.get(slice(0, 45), limit=10))
        self.assertIsInstance(got, list)
        self.assertIsInstance(got, HistoryEntries)
        self.assertEqual(len(got), 10)
        self.assertEqual(got.remainder, 35)
        self.assertIn("35 more", got.note)
        self.assertEqual(got[0], Entry(id="a1b2c3d4", turn=3, role="user", tokens=40, text="hello"))

    def test_edit_encodes_regions_and_defaults_the_anchor(self):
        history, recorder = history_namespace(
            {"history.edit": {"status": "dropped 29, replaced 1, inserted 1 \u2014 live 89,005 / 200,000"}}
        )
        status = asyncio.run(history.edit([
            {"op": "drop", "at": slice(12, 41)},
            {"op": "replace", "at": 41, "text": "review"},
            {"op": "insert", "text": "STATE", "role": "notes"},
            {"op": "insert", "text": "later", "role": "user", "after": "a1b2c3d4"},
        ]))
        self.assertEqual(status, "dropped 29, replaced 1, inserted 1 \u2014 live 89,005 / 200,000")
        self.assertEqual(recorder.calls[0][1]["changes"], [
            {"op": "drop", "at": [12, 41]},
            {"op": "replace", "at": 41, "text": "review"},
            {"op": "insert", "text": "STATE", "role": "notes", "after": -1},
            {"op": "insert", "text": "later", "role": "user", "after": "a1b2c3d4"},
        ])

    def test_edit_rejects_a_replace_region_before_the_round_trip(self):
        history, recorder = history_namespace({"history.edit": {"status": "unused"}})
        with self.assertRaises(TypeError):
            asyncio.run(history.edit([{"op": "replace", "at": slice(0, 2), "text": "x"}]))
        with self.assertRaises(TypeError):
            asyncio.run(history.edit([{"op": "drop", "at": slice(0, 2, 1)}]))
        with self.assertRaises(TypeError):
            asyncio.run(history.get(1.5))
        self.assertEqual(recorder.calls, [])

    def test_index_parses_the_totals_and_rows(self):
        history, _ = history_namespace({"history.index": {
            "entries": 615,
            "live_tokens": 89_000,
            "window": 200_000,
            "over_by": 0,
            "hidden": 29,
            "hidden_tokens": 4_100,
            "rows": [{"id": "a1b2c3d4", "turn": 12, "role": "assistant", "tokens": 4_100,
                      "share": 0.046, "preview": "review: 28 frames"}],
        }})
        index = asyncio.run(history.index("tokens", limit=5, role="assistant", query="frames"))
        self.assertEqual(index.entries, 615)
        self.assertEqual(index.live_tokens, 89_000)
        self.assertEqual(index.over_by, 0)
        self.assertEqual(index.hidden, 29)
        self.assertEqual(index.rows[0], IndexRow(
            id="a1b2c3d4", turn=12, role="assistant", tokens=4_100, share=0.046,
            preview="review: 28 frames",
        ))

    def test_path_returns_entries_in_order(self):
        second = {"id": "", "turn": 4, "role": "notes", "tokens": 2, "text": "STATE"}
        history, _ = history_namespace({"history.path": {"entries": [ENTRY, second]}})
        entries = asyncio.run(history.path())
        self.assertEqual([entry.role for entry in entries], ["user", "notes"])
        self.assertEqual(entries[1].id, "")


class HistorySnapshotTest(unittest.TestCase):
    def test_live_history_references_are_never_snapshotted(self):
        import tempfile

        from rlm import repl

        self.assertIn("history", repl._ALWAYS_SKIP)
        self.assertIn("history", repl._RESTORE_SKIP)
        self.assertIn("_prime_agent_history_namespace", repl._RESTORE_SKIP)
        namespace = HistoryNamespace("session-a")
        ns = {"history": namespace, "alias": namespace, "keep": {"answer": 42}}
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "snapshot")
            result = repl._snapshot_state(ns, path, path + ".json", 1 << 20, 1 << 20)
        self.assertEqual(result["saved"], ["keep"])
        self.assertEqual({entry["name"] for entry in result["skipped"]}, {"alias"})
        self.assertTrue(
            all("live history references" in entry["reason"] for entry in result["skipped"])
        )


if __name__ == "__main__":
    unittest.main()
