#!/usr/bin/env python3
import unittest

from summarize_agent_resources import summarize


class SummaryTests(unittest.TestCase):
    def test_birth_exit_reuse_phase_and_sibling_accounting(self):
        rows = []
        def sample(index, phase, members):
            rows.extend({"sample": str(index), "phase": phase, "row_type": "process",
                         "pid": str(pid), "ppid": str(ppid), "start_id": start,
                         "cpu_seconds": str(cpu), "rss_kib": "1024"}
                        for pid, ppid, start, cpu in members)
            rows.append({"sample": str(index), "phase": phase, "row_type": "tree",
                         "process_count": str(len(members)), "rss_kib": str(len(members) * 1024)})
        sample(0, "idle", [(1, 0, "a", 10), (2, 1, "b", 5), (3, 1, "c", 4)])
        sample(1, "root_tool", [(1, 0, "a", 11), (2, 1, "b", 6), (3, 1, "c", 6), (4, 2, "d", 2)])
        sample(2, "root_tool", [(1, 0, "a", 12), (2, 1, "b", 8), (3, 1, "c", 7)])
        sample(3, "child_one_tool", [(1, 0, "a", 13), (2, 1, "b", 9), (3, 1, "c", 9), (4, 3, "e", 3)])
        receipt = {"sessions": [{"sessionName": "measure-root", "workerPid": 2},
                                {"sessionName": "measure-one", "workerPid": 3}],
                   "turns": [{"phase": "root_tool", "wall_seconds": 1, "tree_cpu_seconds": 7},
                             {"phase": "child_one_tool", "wall_seconds": 2, "tree_cpu_seconds": 6}]}
        result = summarize(rows, receipt)
        self.assertEqual(result["phases"]["root_tool"], {
            "samples": 2, "process_count_min": 3, "process_count_max": 4,
            "tree_rss_median_mib": 3.5, "tree_rss_max_mib": 4.0,
        })
        self.assertEqual([(row["sampled_tree_cpu_seconds"], row["sampled_agent_subtree_cpu_seconds"],
                           row["agent_subtree_peak_rss_mib"]) for row in result["turns"]],
                         [(10.0, 5.0, 2.0), (7.0, 5.0, 2.0)])
        self.assertEqual(result["turns"][0]["boundary_tree_cpu_seconds"], 7)


if __name__ == "__main__":
    unittest.main()
