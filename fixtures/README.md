# Media fixtures

Deliberately not committed: the test suite and the soak script generate them with
ffmpeg on first run, so the handoff stays small and every clip is reproducible from
its generator rather than trusted as a blob.

* `integration_320p_5s.mp4` — built by `tests/test_tenzorpipe_integration.py`
  (320x240, 30 fps, 2 s GOP, B-frames, H.264 + AAC, 5 s).
* `soak_720p_60s.mp4` — built by `scripts/run_integration_soak.sh`
  (1280x720, 30 fps, 60 s), which yields 120 epochs at the default 0.5 s window.
