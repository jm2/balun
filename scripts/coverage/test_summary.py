"""Guard against misleading or silently empty coverage ratchets."""

import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import summarize


class CoverageTests(unittest.TestCase):
    def test_ratchet_rejects_scope_loss_new_misses_and_silent_code_deletion(self):
        before = {"region": summarize.metric(8, 10, [])}
        for after in (summarize.metric(7, 10, []), summarize.metric(8, 9, [])):
            self.assertEqual(summarize.regressions({"region": after}, before), ["region"])
        self.assertEqual(summarize.regressions({"region": summarize.metric(10, 12, [])}, before), [])
        with self.assertRaises(ValueError):
            summarize.regressions({}, before)
        with self.assertRaises(ValueError):
            summarize.metric(0, 0, [])

    def test_rust_deduplicates_regions_and_excludes_tests_and_noncode_regions(self):
        scope = {"rust": {"src/fixture.rs": "admission"}, "rust_excluded_names": ["::tests::"]}
        source = str(summarize.ROOT / "src/fixture.rs")
        functions = [
            {"name": "first", "filenames": [source], "regions": [[1, 1, 1, 9, 0, 0, 0, 0]]},
            {"name": "second", "filenames": [source], "regions": [[1, 1, 1, 9, 1, 0, 0, 0], [2, 1, 2, 9, 0, 0, 0, 3]]},
            {"name": "third", "filenames": [source], "regions": [[3, 1, 3, 9, 1, 0, 0, 0]]},
        ]
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR") or "/var/tmp") as directory:
            path = Path(directory) / "rust.json"
            path.write_text(json.dumps({"data": [{"functions": functions}]}))
            demangled = mock.Mock(stdout="fixture::first\nfixture::second\nfixture::tests::third\n")
            with mock.patch.object(summarize, "SCOPE", scope), \
                    mock.patch.object(summarize.subprocess, "run", return_value=demangled):
                report = summarize.rust_metrics(path)
            self.assertEqual(report["rust-regions:src/fixture.rs"], summarize.metric(1, 1, []))
            with mock.patch.object(summarize, "SCOPE", scope), \
                    mock.patch.object(summarize.subprocess, "run", return_value=mock.Mock(stdout="_Runparsed\n")):
                with self.assertRaises(ValueError):
                    summarize.rust_metrics(path)

    def test_powershell_reports_actual_lines_without_claiming_branch_measurement(self):
        document = '''<coverage><packages><package><classes>
          <class filename="scripts/gate.ps1" branch-rate="1"><methods>
            <method name="Gate"><lines>
              <line number="5" hits="2"/><line number="7" hits="0"/>
            </lines></method>
          </methods></class>
        </classes></package></packages></coverage>'''
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR") or "/var/tmp") as directory:
            path = Path(directory) / "powershell.xml"
            path.write_text(document)
            with mock.patch.object(summarize, "SCOPE", {"powershell": {"scripts/gate.ps1": ["Gate"]}}):
                report = summarize.powershell_metrics(path)
            self.assertEqual(report, {"powershell-lines:scripts/gate.ps1:Gate": summarize.metric(1, 2, [7])})
            with mock.patch.object(summarize, "SCOPE", {"powershell": {"scripts/gate.ps1": ["Missing"]}}):
                with self.assertRaises(KeyError):
                    summarize.powershell_metrics(path)


if __name__ == "__main__":
    unittest.main()
