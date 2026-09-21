"""Phase 4B acceptance: TenzorPipe's decoder writing straight into TenzorBus slots.

This is the live path, with no `.tenzor` file in the middle and no producer copy
anywhere:

    MP4 -> H.264 decode -> resize --writes into--> TenzorBus slot -> N consumers

The reference for every assertion is the `.tenzor` file the *normal* path produces
for the same clip. That is the comparison that matters: it asks whether letting
the decoder write into shared memory changes the tensors, and the answer has to
be no, byte for byte.

Two structural claims are checked rather than asserted in prose:

* `producer_copies == 0` and `destinations_taken == epochs` from the bridge, so
  the engine really took the destination for every epoch.
* the slot each consumer read from equals the slot the decoder wrote into, per
  sequence. Combined with the consumers' own check that their NumPy/PyTorch view
  address equals the slot payload address, that closes the loop: the bytes the
  consumer read are the bytes the decoder wrote.

The whole module skips without the bridge binary, so a checkout that has not run
`scripts/build_bridge.sh` still has a green suite.
"""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DEMO = REPO / "demos" / "tenzorpipe_to_bus.py"
INGEST = REPO / "integration" / "bridge" / "target" / "release" / "tenzorbus-ingest"
FIXTURE_DIR = REPO / "fixtures"
FIXTURE = FIXTURE_DIR / "integration_320p_5s.mp4"

HAVE_TENZORPIPE = importlib.util.find_spec("tenzorpipe") is not None
HAVE_TENZORBUS_RS = importlib.util.find_spec("tenzorbus_rs") is not None
HAVE_FFMPEG = shutil.which("ffmpeg") is not None
HAVE_BRIDGE = INGEST.exists()

ROLES = ["detector", "embedder", "preview"]


def build_fixture() -> Path:
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


def env_with_src() -> dict:
    env = dict(os.environ)
    env["PYTHONPATH"] = os.pathsep.join(
        [str(REPO / "src")] + ([env["PYTHONPATH"]] if env.get("PYTHONPATH") else [])
    )
    return env


def last_json(text: str):
    decoder, i, found = json.JSONDecoder(), 0, None
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
        found = obj
        i = end
    return found


@unittest.skipUnless(HAVE_BRIDGE, "run scripts/build_bridge.sh (needs a patched TenzorPipe)")
@unittest.skipUnless(HAVE_TENZORPIPE, "tenzorpipe not installed")
@unittest.skipUnless(HAVE_TENZORBUS_RS, "run scripts/build_rust.sh")
@unittest.skipUnless(HAVE_FFMPEG, "ffmpeg not available to build the media fixture")
class LiveDirectWriteTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.media = build_fixture()
        cls.work = tempfile.TemporaryDirectory(prefix="tenzorbus-live-")
        cls.reference = Path(cls.work.name) / "reference.tenzor"
        # The reference is produced by the ordinary, unmodified `.tenzor` path.
        engine = os.environ.get("TENZORPIPE_BIN")
        if engine:
            subprocess.run(
                [engine, "-i", str(cls.media), "-o", str(cls.reference), "--quiet"],
                check=True,
            )
        else:
            import tenzorpipe as tp

            tp.ingest(cls.media, cls.reference)
        import tenzorpipe as tp

        with tp.load(cls.reference) as data:
            cls.epochs = len(data)
        assert cls.epochs > 1, "fixture produced too few epochs to be a test"

    @classmethod
    def tearDownClass(cls):
        cls.work.cleanup()

    def run_live(self, consumers=3, slots=8, extra_ingest=(), sleep_max_ms=0.0,
                 die_after=0, timeout=300):
        """Start consumers, stream through the bridge, return (bridge, [consumers])."""
        ring = f"live_{os.getpid()}_{time.monotonic_ns() % 1_000_000}"
        env = env_with_src()
        children = []
        for i in range(consumers):
            cmd = [
                sys.executable, str(DEMO), "consume",
                "--backend", "rust", "--ring", ring,
                "--tenzor", str(self.reference),
                "--expect", str(self.epochs),
                "--role", ROLES[i % len(ROLES)],
                "--timeout", "120",
            ]
            if sleep_max_ms:
                cmd += ["--sleep-max-ms", str(sleep_max_ms)]
            if die_after and i == 0:
                cmd += ["--die-after", str(die_after)]
            children.append(subprocess.Popen(cmd, stdout=subprocess.PIPE, text=True, env=env))

        ingest = subprocess.run(
            [
                str(INGEST), "stream",
                "--media", str(self.media),
                "--ring", ring,
                "--slots", str(slots),
                "--await-consumers", str(consumers),
                "--timeout-s", "120",
                *extra_ingest,
            ],
            capture_output=True, text=True, timeout=timeout, env=env,
        )
        reports = []
        for child in children:
            out, _ = child.communicate(timeout=timeout)
            report = last_json(out) or {"error": "no report"}
            report["exit_code"] = child.returncode
            reports.append(report)

        try:
            import tenzorbus_rs as tb

            tb.unlink(ring)
        except Exception:
            pass

        bridge = last_json(ingest.stdout) or {}
        bridge["exit_code"] = ingest.returncode
        bridge["stderr_tail"] = ingest.stderr[-1500:]
        return bridge, reports

    def assert_consumer_clean(self, report, expect_slots):
        self.assertNotIn("error", report, report)
        self.assertEqual(report["received"], self.epochs, report)
        self.assertEqual(report["content_mismatches"], 0, "tensor differs from the .tenzor tensor")
        self.assertEqual(report["shape_mismatches"], 0)
        self.assertEqual(report["dtype_mismatches"], 0)
        self.assertEqual(report["timestamp_mismatches"], 0, "media timestamp did not survive")
        self.assertEqual(report["sequence_gaps"], 0)
        self.assertEqual(report["out_of_order"], 0)
        self.assertEqual(report["not_zero_copy"], 0, "the view stopped aliasing the slot")
        self.assertEqual(report["lease_violations"], 0)
        self.assertEqual(report["first_sequence"], 1)
        self.assertEqual(report["last_sequence"], self.epochs)
        self.assertEqual(report["exit_code"], 0, report)
        # The decisive structural check.
        self.assertEqual(
            {tuple(pair) for pair in report["slots"]},
            expect_slots,
            "the consumer read a different slot than the decoder wrote into",
        )

    def test_the_decoder_writes_into_the_slots_the_consumers_read(self):
        bridge, reports = self.run_live(consumers=3, slots=8)
        self.assertEqual(bridge.get("exit_code"), 0, bridge)
        self.assertEqual(bridge["epochs"], self.epochs)
        self.assertEqual(bridge["published"], self.epochs)
        self.assertEqual(bridge["mode"], "direct")
        # Zero copies on the whole path, and a destination taken every time.
        self.assertEqual(bridge["producer_copies"], 0, bridge)
        self.assertEqual(bridge["destinations_taken"], self.epochs, bridge)
        self.assertEqual(bridge["frame_bytes"], 3 * 224 * 224 * 4)
        self.assertEqual(bridge["ring_dropped"], 0)

        expect_slots = {tuple(pair) for pair in bridge["slot_trace"]}
        self.assertEqual(len(expect_slots), self.epochs)
        self.assertEqual(len(reports), 3)
        for report in reports:
            with self.subTest(role=report.get("role")):
                self.assert_consumer_clean(report, expect_slots)

        torch_views = sum(r["zero_copy_torch"] for r in reports)
        numpy_views = sum(r["zero_copy_numpy"] for r in reports)
        self.assertEqual(torch_views, self.epochs)
        self.assertEqual(numpy_views, 2 * self.epochs)

    def test_the_copy_path_delivers_the_same_tensors(self):
        """Declining the destination must change nothing a consumer can see."""
        bridge, reports = self.run_live(consumers=2, slots=8, extra_ingest=("--copy-path",))
        self.assertEqual(bridge.get("exit_code"), 0, bridge)
        self.assertEqual(bridge["mode"], "copy")
        self.assertEqual(bridge["destinations_taken"], 0, bridge)
        self.assertEqual(bridge["producer_copies"], self.epochs, bridge)
        expect_slots = {tuple(pair) for pair in bridge["slot_trace"]}
        for report in reports:
            with self.subTest(role=report.get("role")):
                self.assert_consumer_clean(report, expect_slots)

    def test_verification_detects_a_corrupted_slot(self):
        """Negative control. A gate that cannot fail proves nothing."""
        bridge, reports = self.run_live(
            consumers=2, slots=8, extra_ingest=("--corrupt-epoch", "3")
        )
        self.assertEqual(bridge.get("exit_code"), 0, bridge)
        self.assertEqual(bridge["producer_copies"], 0)
        for report in reports:
            with self.subTest(role=report.get("role")):
                self.assertEqual(
                    report["content_mismatches"], 1,
                    "the deliberately corrupted epoch was not caught",
                )
                self.assertEqual(report["timestamp_mismatches"], 0, report)
                self.assertNotEqual(report["exit_code"], 0)

    def test_backpressure_reaches_the_decoder(self):
        """Two slots and slow consumers: the decoder is paced, not queued."""
        bridge, reports = self.run_live(consumers=3, slots=2, sleep_max_ms=6.0)
        self.assertEqual(bridge.get("exit_code"), 0, bridge)
        self.assertEqual(bridge["published"], self.epochs)
        self.assertEqual(bridge["producer_copies"], 0)
        self.assertEqual(bridge["ring_dropped"], 0)
        expect_slots = {tuple(pair) for pair in bridge["slot_trace"]}
        for report in reports:
            with self.subTest(role=report.get("role")):
                self.assert_consumer_clean(report, expect_slots)

    def test_a_consumer_crash_does_not_stall_the_decoder(self):
        """SIGKILL a consumer holding a lease on a slot the decoder wrote into."""
        bridge, reports = self.run_live(consumers=3, slots=2, die_after=2)
        self.assertEqual(bridge.get("exit_code"), 0, bridge)
        self.assertEqual(
            bridge["published"], self.epochs,
            "the decoder stalled behind a dead consumer",
        )
        self.assertGreaterEqual(bridge["ring_reaped"], 1, "the dead consumer was never reaped")

        survivors = [r for r in reports if r.get("exit_code") == 0]
        killed = [r for r in reports if r.get("exit_code") != 0]
        self.assertEqual(len(killed), 1, reports)
        self.assertEqual(len(survivors), 2, reports)
        expect_slots = {tuple(pair) for pair in bridge["slot_trace"]}
        for report in survivors:
            self.assert_consumer_clean(report, expect_slots)
        for report in killed:
            # Everything it did see was still correct.
            self.assertEqual(report["content_mismatches"], 0, report)
            self.assertEqual(report["timestamp_mismatches"], 0, report)


if __name__ == "__main__":
    unittest.main()
