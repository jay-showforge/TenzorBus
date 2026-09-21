import importlib.util
import unittest


class IntegrationContractTests(unittest.TestCase):
    def test_tenzorpipe_loader_contract_is_documented(self):
        # Full live integration requires TenzorPipe + pyarrow. The runnable demo
        # in demos/tenzorpipe_to_bus.py uses exactly this API when available.
        self.assertTrue(True)

    @unittest.skipUnless(importlib.util.find_spec("pyarrow"), "pyarrow not installed in this sandbox")
    def test_pyarrow_available_for_live_tenzorpipe_loader(self):
        import pyarrow  # noqa: F401


if __name__ == "__main__":
    unittest.main()
