#!/usr/bin/env python3
import csv
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import sample_agent_resources as sampler


class ResourceSamplerTests(unittest.TestCase):
    def process(self, pid=10, ppid=1, start="100", cpu=1.0, rss=1024):
        return sampler.Process(pid, ppid, start, "worker", cpu, rss)

    def test_proc_stat_uses_own_cpu_and_handles_parentheses(self):
        fields = ["S", "1"] + ["0"] * 20
        fields[11:15] = ["150", "25", "900", "800"]
        fields[19] = "1234"
        fields[21] = "42"
        actual = sampler.parse_proc_stat("10 (worker (busy)) " + " ".join(fields), 100, 4096)
        self.assertEqual(actual, sampler.Process(10, 1, "1234", "worker (busy)", 1.75, 168))

    def test_tree_includes_grandchildren_and_excludes_unrelated(self):
        processes = {p.pid: p for p in [
            self.process(), self.process(11, 10), self.process(12, 11),
            self.process(99, 1),
        ]}
        self.assertEqual(sampler.descendants(processes, 10), [processes[p] for p in (10, 11, 12)])
        self.assertEqual(sampler.descendants(processes, 50), [])

    def test_delta_identity_and_tree_completeness(self):
        members = [self.process(cpu=2.5), self.process(11, 10, "200", 4, 512)]
        previous = {(10, "100"): 2.0, (11, "199"): 100.0}
        rows = sampler.sample_rows(members, previous, {"root_pid": 10})
        self.assertEqual(
            [(r["cpu_delta_seconds"], r["cpu_delta_complete"]) for r in rows],
            [(0.5, 1), ("", 0), (0.5, 0)],
        )
        self.assertEqual((rows[-1]["rss_kib"], rows[-1]["cpu_seconds"]), (1536, 6.5))
        self.assertTrue(all(r["root_pid"] == 10 for r in rows))

    def test_ps_start_identity_and_cpu_formats(self):
        text = " 10 1 1-02:03:04 512 Sat Nov 21 15:00:00 2026 worker\n"
        self.assertEqual(sampler.parse_ps(text), {
            10: sampler.Process(10, 1, "Sat Nov 21 15:00:00 2026", "worker", 93784, 512),
        })
        self.assertEqual(sampler.parse_cpu_time("02:03"), 123)

    def test_cli_rereads_phase_and_stops_on_root_reuse(self):
        first = self.process()
        second = self.process(cpu=2.0)
        reused = self.process(start="999")
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "samples.csv"
            phase = Path(directory) / "phase"
            phase.write_text("idle")
            def next_phase(delay):
                phase.write_text("active")
            with patch.object(sampler, "read_proc", side_effect=[
                {10: first}, {10: first}, {10: second}, {10: reused},
            ]), patch.object(sampler.time, "sleep", side_effect=next_phase):
                result = sampler.main([
                    "--pid", "10", "--backend", "proc", "--output", str(output),
                    "--phase-file", str(phase),
                ])
            with output.open(newline="") as handle:
                rows = list(csv.DictReader(handle))
            self.assertEqual(result, 0)
            self.assertEqual([(r["phase"], r["row_type"]) for r in rows], [
                ("idle", "process"), ("idle", "tree"),
                ("active", "process"), ("active", "tree"),
            ])
            self.assertEqual(rows[2]["cpu_delta_seconds"], "1.0")
            self.assertGreaterEqual(float(rows[2]["sample_elapsed_seconds"]), 0)
            self.assertEqual(rows[0]["pid"], "10")

    def test_sigterm_closes_complete_sample_and_restores_handler(self):
        original = sampler.signal.getsignal(sampler.signal.SIGTERM)
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "samples.csv"
            with patch.object(sampler, "read_proc", return_value={10: self.process()}), patch.object(
                sampler.time, "sleep", side_effect=lambda delay: sampler.signal.raise_signal(sampler.signal.SIGTERM),
            ):
                self.assertEqual(sampler.main([
                    "--pid", "10", "--backend", "proc", "--output", str(output),
                ]), 0)
            with output.open(newline="") as handle:
                rows = list(csv.DictReader(handle))
            self.assertEqual([r["row_type"] for r in rows], ["process", "tree"])
            self.assertEqual(sampler.signal.getsignal(sampler.signal.SIGTERM), original)

    def test_duration_stops_after_deadline(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "samples.csv"
            with patch.object(sampler, "read_proc", return_value={10: self.process()}), patch.object(
                sampler.time, "monotonic", side_effect=[0, 0, 0, 0, 2],
            ), patch.object(sampler.time, "sleep"):
                sampler.main([
                    "--pid", "10", "--backend", "proc", "--duration", "1",
                    "--interval", "0.05", "--output", str(output),
                ])
            with output.open(newline="") as handle:
                rows = list(csv.DictReader(handle))
            self.assertEqual(len(rows), 2)

    def test_output_is_not_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "samples.csv"
            output.write_text("keep")
            with patch.object(sampler, "read_proc", return_value={10: self.process()}):
                with self.assertRaises(FileExistsError):
                    sampler.main(["--pid", "10", "--backend", "proc", "--output", str(output)])
            self.assertEqual(output.read_text(), "keep")

    def test_interval_rejects_nonpositive_and_nonfinite(self):
        for value in ("0", "-1", "nan", "inf"):
            with self.subTest(value=value), self.assertRaises(sampler.argparse.ArgumentTypeError):
                sampler.positive(value)


if __name__ == "__main__":
    unittest.main()
