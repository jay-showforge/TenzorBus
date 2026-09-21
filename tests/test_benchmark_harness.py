import importlib.util
import pathlib
import unittest
from unittest import mock


MODULE_PATH = pathlib.Path(__file__).parents[1] / "benchmarks" / "benchmark_matrix.py"
SPEC = importlib.util.spec_from_file_location("benchmark_matrix", MODULE_PATH)
benchmark_matrix = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark_matrix)


def complete_case(path, payload_bytes, consumers, transport=None):
    case = {
        "path": path,
        "payload_bytes": payload_bytes,
        "consumers": consumers,
        "iterations": 40,
        "delivered_per_consumer": [40] * consumers,
        "latency_median_ms": 0.1,
        "latency_p95_ms": 0.2,
        "latency_p99_ms": 0.3,
        "publications_per_s": 1000.0,
        "all_consumers_received_everything": True,
    }
    if path == "unix_socket":
        case["unix_socket_transport"] = transport
    return case


class BenchmarkHarnessTests(unittest.TestCase):
    def test_seccomp_pathname_failure_uses_real_socketpair(self):
        left = mock.Mock()
        right = mock.Mock()
        with mock.patch.object(
            benchmark_matrix.socket, "socket", side_effect=PermissionError("blocked")
        ), mock.patch.object(
            benchmark_matrix.socket, "socketpair", return_value=(left, right)
        ):
            selected = benchmark_matrix.select_unix_socket_transport("/tmp")
        self.assertEqual(selected, benchmark_matrix.UNIX_SOCKETPAIR)
        left.close.assert_called_once_with()
        right.close.assert_called_once_with()

    def test_complete_seven_case_transport_matrix_passes(self):
        path = "unix_socket"
        transport = benchmark_matrix.UNIX_SOCKETPAIR
        plan = [
            (path, 64 * 1024, 1),
            (path, benchmark_matrix.REAL_FRAME, 1),
            (path, 4 << 20, 1),
            (path, 16 << 20, 1),
            (path, benchmark_matrix.REAL_FRAME, 2),
            (path, benchmark_matrix.REAL_FRAME, 4),
            (path, benchmark_matrix.REAL_FRAME, 8),
        ]
        report = {
            "cases": [complete_case(*entry, transport=transport) for entry in plan]
        }
        benchmark_matrix.validate_matrix(report, plan, [path], transport)

    def test_incomplete_delivery_is_rejected(self):
        case = complete_case("fifo", benchmark_matrix.REAL_FRAME, 2)
        case["delivered_per_consumer"] = [40, 39]
        case["all_consumers_received_everything"] = False
        with self.assertRaisesRegex(ValueError, "incomplete delivery"):
            benchmark_matrix.validate_case(case)

    def test_unidentified_unix_transport_is_rejected(self):
        case = complete_case("unix_socket", benchmark_matrix.REAL_FRAME, 1)
        with self.assertRaisesRegex(ValueError, "AF_UNIX/SOCK_STREAM"):
            benchmark_matrix.validate_case(case)


if __name__ == "__main__":
    unittest.main()
