import hashlib
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


TENZOR = os.environ.get("TENZORPIPE_BIN")
# This module builds its fixture with ffmpeg. Without that guard the test raised
# FileNotFoundError instead of skipping, which is a different and much more
# confusing failure than the one every neighbouring module produces.
HAVE_FFMPEG = shutil.which("ffmpeg") is not None


@unittest.skipUnless(TENZOR and Path(TENZOR).exists(), "set TENZORPIPE_BIN to a real TenzorPipe release binary")
@unittest.skipUnless(HAVE_FFMPEG, "ffmpeg not available to build the media fixture")
class TenzorPipeReleaseTests(unittest.TestCase):
    def test_real_release_ingest_is_deterministic_arrow(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            src = td / "fixture.mp4"
            subprocess.run([
                "ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
                "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=2",
                "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=2",
                "-c:v", "libx264", "-pix_fmt", "yuv420p", "-g", "30", "-bf", "2",
                "-c:a", "aac", "-shortest", str(src),
            ], check=True)
            outs = [td / "a.tenzor", td / "b.tenzor"]
            for out in outs:
                subprocess.run([TENZOR, "-i", str(src), "-o", str(out), "--video-workers", "1"], check=True)
                blob = out.read_bytes()
                self.assertGreater(len(blob), 1024)
                self.assertEqual(blob[:6], b"ARROW1")
                self.assertEqual(blob[-6:], b"ARROW1")
            ha = hashlib.sha256(outs[0].read_bytes()).hexdigest()
            hb = hashlib.sha256(outs[1].read_bytes()).hexdigest()
            self.assertEqual(ha, hb)


if __name__ == "__main__":
    unittest.main()
