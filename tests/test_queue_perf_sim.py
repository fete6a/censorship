"""Deterministic latency and request-count simulation; not a production benchmark.

Run: python3 -m unittest tests/test_queue_perf_sim.py -v
The scenario uses deterministic service times, so results are reproducible.
"""
import unittest

def simulate(chats, policy_ms=55, model_ms=180, d1_ms=8, fail_at=()):
    """Ordered, independent processing; one policy fetch per group per batch."""
    result = {}
    for reuse in (False, True):
        cache = set()
        calls = {"model": 0, "d1": 0, "policy": 0, "errors": 0, "acked": 0, "retried": 0, "ms": 0}
        for i, chat in enumerate(chats):
            calls["d1"] += 4  # baseline state checks/writes; PR #4 unaffected
            calls["ms"] += 4 * d1_ms
            if not reuse or chat not in cache:
                calls["policy"] += 1
                calls["ms"] += policy_ms
                cache.add(chat)
            calls["model"] += 1
            calls["ms"] += model_ms
            if i in fail_at:
                calls["errors"] += 1
                calls["retried"] += 1
            else:
                calls["acked"] += 1
        result["cached" if reuse else "baseline"] = calls
    return result

class QueuePerfSimulation(unittest.TestCase):
    def test_single_group_twenty(self):
        d = simulate([-1001] * 20)
        a,b = d["baseline"],d["cached"]
        self.assertEqual((a["model"],b["model"]), (20,20))
        self.assertEqual((a["policy"],b["policy"]), (20,1))
        self.assertEqual((a["d1"],b["d1"]), (80,80))
        self.assertEqual((a["ms"],b["ms"]), (5340,4295))
        self.assertEqual((a["acked"],b["acked"]), (20,20))

    def test_two_groups_and_retry_isolation(self):
        d=simulate([-1001]*12+[-1002]*8,fail_at=(4,17))
        a,b=d["baseline"],d["cached"]
        self.assertEqual((a["policy"],b["policy"]), (20,2))
        self.assertEqual((a["model"],b["model"]), (20,20))
        self.assertEqual((a["d1"],b["d1"]), (80,80))
        self.assertEqual((a["retried"],b["retried"]), (2,2))
        self.assertEqual((a["acked"],b["acked"]), (18,18))
        self.assertEqual((a["ms"],b["ms"]), (5340,4350))

if __name__ == "__main__":
    unittest.main()
