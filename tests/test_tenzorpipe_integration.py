"""Phase 4A acceptance: real TenzorPipe media through the real TenzorBus ring.

Every test here runs the actual TenzorPipe engine over an actual H.264/AAC MP4 and
drives real consumer processes. Nothing is mocked. The whole module skips if
TenzorPipe or ffmpeg is unavailable, so a checkout without them still has a green
suite, but the tests never substitute synthetic tensors for real ones.

The gate is deliberately falsifiable: `test_verification_detects_a_misordered_publication`
injects a fault and requires the consumers to catch it. Without that, "zero
mismatches" would prove nothing.
"""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DEMO = REPO / "demos" / "tenzorpipe_to_bus.py"
FIXTURE_DIR = REPO / "fixtures"
FIXTURE = FIXTURE_DIR / "integration_320p_5s.mp4"

HAVE_TENZORPIPE = importlib.util.find_spec("tenzorpipe") is not None
HAVE_TENZORBUS_RS = importlib.util.find_spec("tenzorbus_rs") is not None
HAVE_FFMPEG = shutil.which("ffmpeg") is not None


def build_fixture() -> Path:
    """A real H.264/AAC clip with B-frames and a 2-second GOP."""
    if FIXTURE.exists():
        return FIXTURE
    FIXTURE_DIR.mkdir(exist_ok=True)
    subprocess.run(
        [
            "ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=5",
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=5",
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-g", "60", "-bf", "2",
            "-c:a", "aac", "-shortest", str(FIXTURE),
        ],
        check=True,
    )
    return FIXTURE


def run_demo(*args, timeout=900):
    """Run the integration demo and return (returncode, orchestrator report)."""
    env = dict(os.environ)
    env["PYTHONPATH"] = os.pathsep.join(
        [str(REPO / "src")] + ([env["PYTHONPATH"]] if env.get("PYTHONPATH") else [])
    )
    proc = subprocess.run(
        [sys.executable, str(DEMO), "run", *args],
        capture_output=True, text=True, timeout=timeout, env=env,
    )
    report = None
    decoder = json.JSONDecoder()
    text, i = proc.stdout, 0
    while i < len(text):
        while i < len(text) and text[i] != "{":
            i += 1
        if i >= len(text):
            break
        try:
            obj, end = decoder.raw_decode(text, i)
        except json.JSONDecodeError:
            i += 1
            continue
        if isinstance(obj, dict) and "consumer_reports" in obj:
            report = obj
        i = end
    if report is None:
        raise AssertionError(
            f"demo produced no report (rc={proc.returncode})\n"
            f"stdout tail:\n{proc.stdout[-2000:]}\nstderr tail:\n{proc.stderr[-2000:]}"
        )
    return proc.returncode, report


@unittest.skipUnless(HAVE_TENZORPIPE, "tenzorpipe not installed; see RUST_PHASE_REPORT.md")
@unittest.skipUnless(HAVE_FFMPEG, "ffmpeg not available to build the media fixture")
class TenzorPipeIntegrationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.media = str(build_fixture())

    def assert_clean(self, report, *, expect_consumers):
        self.assertEqual(report["published"], report["epochs"])
        self.assertGreater(report["epochs"], 1, "fixture produced too few epochs to be a test")
        self.assertEqual(len(report["consumer_reports"]), expect_consumers)
        for c in report["consumer_reports"]:
            with self.subTest(role=c["role"], pid=c.get("pid")):
                self.assertNotIn("error", c, c)
                self.assertEqual(c["received"], report["epochs"])
                self.assertEqual(c["content_mismatches"], 0, "payload differs from TenzorPipe's tensor")
                self.assertEqual(c["shape_mismatches"], 0)
                self.assertEqual(c["dtype_mismatches"], 0)
                self.assertEqual(c["timestamp_mismatches"], 0, "media timestamp did not survive")
                self.assertEqual(c["sequence_gaps"], 0)
                self.assertEqual(c["out_of_order"], 0)
                self.assertEqual(c["not_zero_copy"], 0, "consumer view stopped aliasing the slot")
                self.assertEqual(c["lease_violations"], 0)
                # Exactly-once: N distinct sequences from 1..N with no gaps or repeats.
                self.assertEqual(c["first_sequence"], 1)
                self.assertEqual(c["last_sequence"], report["epochs"])
                self.assertEqual(c["exit_code"], 0)

    @unittest.skipUnless(HAVE_TENZORBUS_RS, "build the Rust extension: scripts/build_rust.sh")
    def test_rust_ring_delivers_real_tensors_to_three_processes(self):
        rc, report = run_demo("--media", self.media, "--consumers", "3", "--backend", "rust")
        self.assertEqual(rc, 0, report)
        self.assert_clean(report, expect_consumers=3)
        self.assertEqual(report["video_shape"], [3, 224, 224])
        self.assertEqual(report["frame_bytes"], 3 * 224 * 224 * 4)
        self.assertEqual(report["ring_stats"]["dropped"], 0)
        # One PyTorch consumer and two NumPy consumers, all aliasing the slot.
        torch_views = sum(c["zero_copy_torch"] for c in report["consumer_reports"])
        numpy_views = sum(c["zero_copy_numpy"] for c in report["consumer_reports"])
        self.assertEqual(torch_views, report["epochs"])
        self.assertEqual(numpy_views, 2 * report["epochs"])

    def test_python_reference_ring_delivers_the_same_tensors(self):
        rc, report = run_demo("--media", self.media, "--consumers", "3", "--backend", "reference")
        self.assertEqual(rc, 0, report)
        self.assert_clean(report, expect_consumers=3)

    @unittest.skipUnless(HAVE_TENZORBUS_RS, "build the Rust extension: scripts/build_rust.sh")
    def test_verification_detects_a_misordered_publication(self):
        """Negative control. A gate that cannot fail proves nothing."""
        rc, report = run_demo(
            "--media", self.media, "--consumers", "2",
            "--backend", "rust", "--inject", "swap-frames",
        )
        self.assertEqual(rc, 0, "the injected fault was not caught")
        caught = sum(c["content_mismatches"] for c in report["consumer_reports"])
        self.assertGreater(caught, 0, report)
        for c in report["consumer_reports"]:
            self.assertEqual(c["content_mismatches"], 2, c)
            self.assertNotEqual(c["exit_code"], 0)

    @unittest.skipUnless(HAVE_TENZORBUS_RS, "build the Rust extension: scripts/build_rust.sh")
    def test_verification_detects_a_swapped_timestamp(self):
        rc, report = run_demo(
            "--media", self.media, "--consumers", "2",
            "--backend", "rust", "--inject", "swap-timestamps",
        )
        self.assertEqual(rc, 0, "the injected fault was not caught")
        for c in report["consumer_reports"]:
            self.assertEqual(c["timestamp_mismatches"], 2, c)
            self.assertEqual(c["content_mismatches"], 0, c)

    @unittest.skipUnless(HAVE_TENZORBUS_RS, "build the Rust extension: scripts/build_rust.sh")
    def test_backpressure_on_a_two_slot_ring_with_slow_consumers(self):
        """Three consumers, two slots: the ring is saturated for the whole run."""
        rc, report = run_demo(
            "--media", self.media, "--consumers", "3", "--backend", "rust",
            "--slots", "2", "--sleep-max-ms", "8",
        )
        self.assertEqual(rc, 0, report)
        self.assert_clean(report, expect_consumers=3)
        self.assertEqual(report["slots"], 2)

    @unittest.skipUnless(HAVE_TENZORBUS_RS, "build the Rust extension: scripts/build_rust.sh")
    def test_consumer_crash_mid_stream_does_not_stall_the_media_pipeline(self):
        """SIGKILL a consumer while it holds a lease on a real video tensor.

        The producer must keep publishing every epoch, and the surviving
        consumers must still receive all of them, uncorrupted.
        """
        rc, report = run_demo(
            "--media", self.media, "--consumers", "3", "--backend", "rust",
            "--slots", "2", "--die-after", "3",
        )
        self.assertEqual(rc, 0, report)
        self.assertEqual(report["published"], report["epochs"],
                         "the ring stalled behind a dead consumer")
        self.assertGreaterEqual(report["ring_stats"]["reaped"], 1,
                                "the dead consumer was never reaped")

        killed = [c for c in report["consumer_reports"] if c["exit_code"] not in (0,)]
        survivors = [c for c in report["consumer_reports"] if c["exit_code"] == 0]
        self.assertEqual(len(killed), 1, report["consumer_reports"])
        self.assertEqual(len(survivors), 2, report["consumer_reports"])
        for c in survivors:
            self.assertEqual(c["received"], report["epochs"], c)
            self.assertEqual(c["content_mismatches"], 0, c)
            self.assertEqual(c["timestamp_mismatches"], 0, c)
            self.assertEqual(c["sequence_gaps"], 0, c)
            self.assertEqual(c["out_of_order"], 0, c)
        # Everything the killed consumer did see was still correct.
        for c in killed:
            self.assertEqual(c["content_mismatches"], 0, c)
            self.assertEqual(c["timestamp_mismatches"], 0, c)


if __name__ == "__main__":
    unittest.main()
