#!/usr/bin/env python3
"""Fail-closed native ARM64 TenzorPipe -> TenzorBus integration check."""

from __future__ import annotations

import argparse
import json
import platform
import runpy
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()

    if platform.system() != "Linux" or platform.machine() != "aarch64":
        parser.error(
            "native TenzorPipe validation requires Linux aarch64; "
            f"found {platform.system()} {platform.machine()}"
        )

    namespace = runpy.run_path(str(ROOT / "tests" / "test_live_direct_write.py"))
    test_class = namespace["LiveDirectWriteTests"]
    case = test_class("test_the_decoder_writes_into_the_slots_the_consumers_read")
    test_class.setUpClass()
    try:
        bridge, reports, cleanup = case.run_live(
            consumers=1, slots=4, verify_cleanup=True
        )
        case.assertEqual(bridge.get("exit_code"), 0, bridge)
        case.assertEqual(bridge["mode"], "direct")
        case.assertEqual(bridge["epochs"], test_class.epochs)
        case.assertEqual(bridge["published"], test_class.epochs)
        case.assertEqual(bridge["destinations_taken"], test_class.epochs)
        case.assertEqual(bridge["producer_copies"], 0)
        case.assertEqual(bridge["frame_bytes"], 3 * 224 * 224 * 4)

        case.assertEqual(len(reports), 1)
        consumer = reports[0]
        expected_slots = {tuple(pair) for pair in bridge["slot_trace"]}
        case.assert_consumer_clean(consumer, expected_slots)
        case.assertEqual(consumer["observed_shape"], [3, 224, 224])
        case.assertEqual(consumer["observed_dtype"], "float32")
        case.assertEqual(consumer["zero_copy_numpy"], test_class.epochs)
        case.assertEqual(consumer["zero_copy_torch"], 0)

        import tenzorpipe as tp

        with tp.load(test_class.reference) as data:
            first_batch = data.reader.get_batch(0)
            last_batch = data.reader.get_batch(data.reader.num_record_batches - 1)
            first_ts = (
                int(first_batch.column("video_timestamp_ms")[0].as_py()) * 1_000_000
            )
            last_ts = (
                int(
                    last_batch.column("video_timestamp_ms")[
                        last_batch.num_rows - 1
                    ].as_py()
                )
                * 1_000_000
            )
        case.assertEqual(consumer["first_timestamp_ns"], first_ts)
        case.assertEqual(consumer["last_timestamp_ns"], last_ts)
        case.assertTrue(cleanup["unlink_completed"], cleanup)
        case.assertTrue(cleanup["attach_after_unlink_refused"], cleanup)

        result = {"bridge": bridge, "consumer": consumer, "cleanup": cleanup}
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(result, indent=2))
        print("native ARM64 TenzorPipe integration passed")
        return 0
    finally:
        test_class.tearDownClass()


if __name__ == "__main__":
    raise SystemExit(main())
