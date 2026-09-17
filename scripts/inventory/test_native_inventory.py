#!/usr/bin/env python3
"""Synthetic native inventory admission, content drift and SBOM regressions."""

import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import native_inventory as inventory


def fixtures():
    def component(key, version):
        return {"id": key, "name": key, "version": version,
                "package": {"name": "fixture-" + key, "version": version + "-2",
                            "reference": "https://packages.example/" + key + "/" + version},
                "source": {"reference": "https://source.example/" + key + "/archive",
                           "identity": "sha256:" + "a" * 64},
                "licenses": [{"name": "fixture-license", "reference": "https://source.example/license"}],
                "source_delivery": {"reference": "https://source.example/" + key + "/archive",
                                    "note": "Synthetic source-delivery reference for this fixture."}}
    members = []
    for path, owner in [("bin/core.dll", "runtime"), ("lib/plugins/decoder.dll", "decoder"),
                        ("libexec/scanner.exe", "runtime")]:
        payload = ("synthetic bytes: " + path).encode()
        members.append({"path": path, "component": owner, "size": len(payload),
                        "sha256": hashlib.sha256(payload).hexdigest()})
    observed = {"schema": 1, "platform": "windows-x86_64",
                "artifact": {"name": "balun-windows-x86_64.zip", "version": "0.1.1",
                             "size": 12345, "sha256": "b" * 64},
                "native_files": [{key: value for key, value in member.items() if key != "component"}
                                 for member in members]}
    catalog = {"schema": 1, "components": [component("runtime", "1.20.0"), component("decoder", "2.0")],
               "external_components": [{"id": "system-runtime", "name": "Target system runtime",
                                        "requirement": "Provided by the target operating system",
                                        "provider": "Operating system", "reference": "https://system.example/runtime"}],
               "native_files": members}
    return observed, catalog


class NativeInventoryTests(unittest.TestCase):
    def test_complete_membership_and_content_reach_the_sbom(self):
        observed, catalog = fixtures()
        result = inventory.assemble(observed, catalog)
        bom = inventory.sbom(result)
        self.assertEqual(result["artifact"], observed["artifact"])
        self.assertEqual(bom["metadata"]["component"]["hashes"][0]["content"], "b" * 64)
        files = {member["name"]: member["hashes"][0]["content"]
                 for component in bom["components"] for member in component.get("components", [])}
        self.assertEqual(files, {member["path"]: member["sha256"] for member in observed["native_files"]})
        external = next(item for item in bom["components"] if item["bom-ref"] == "component:system-runtime")
        self.assertNotIn("version", external)
        self.assertNotIn("hashes", external)
        self.assertIn({"name": "balun:management", "value": "externally-managed"}, external["properties"])
        self.assertEqual(bom["compositions"][0]["aggregate"], "incomplete")
        self.assertNotIn("signature", bom)
        self.assertNotIn("formulation", bom)
        self.assertNotIn("declarations", bom)
        # Ordering of package-manager records must not affect the inventory.
        catalog["components"].reverse()
        catalog["native_files"].reverse()
        observed["native_files"].reverse()
        self.assertEqual(result, inventory.assemble(observed, catalog))

    def test_changed_missing_extra_and_unowned_members_fail(self):
        def changed_hash(observed, _):
            observed["native_files"][1]["sha256"] = "f" * 64
        def changed_size(observed, _):
            observed["native_files"][1]["size"] += 1
        def missing(observed, _):
            observed["native_files"].pop()
        def extra(observed, _):
            observed["native_files"].append({"path": "lib/unknown.dll", "size": 1, "sha256": "c" * 64})
        def unknown_owner(_, catalog):
            catalog["native_files"][0]["component"] = "unregistered"
        def external_owner(_, catalog):
            catalog["native_files"][0]["component"] = "system-runtime"
        def absent_component(_, catalog):
            extra = copy.deepcopy(catalog["components"][0]); extra["id"] = "unused"
            catalog["components"].append(extra)
        for mutate in [changed_hash, changed_size, missing, extra, unknown_owner, external_owner, absent_component]:
            with self.subTest(mutate=mutate.__name__):
                observed, catalog = fixtures(); mutate(observed, catalog)
                with self.assertRaises(inventory.Invalid):
                    inventory.assemble(observed, catalog)

    def test_paths_cannot_alias_or_collide(self):
        for path in ["/root/lib.dll", "../lib.dll", "a/../lib.dll", "a//lib.dll", "a/./lib.dll",
                     "C:/lib.dll", "a\\lib.dll", "a/lib.dll:stream", "a/lib.dll.", "a/lib.dll ",
                     "a/CON.dll", "a/lib?.dll", "a/lib\x00.dll", "a/lib\n.dll", "a/líb.dll"]:
            with self.subTest(path=repr(path)), self.assertRaises(inventory.Invalid):
                inventory.member_path(path)
        observed, catalog = fixtures()
        duplicate = dict(observed["native_files"][0]); duplicate["path"] = duplicate["path"].upper()
        observed["native_files"].append(duplicate)
        with self.assertRaises(inventory.Invalid):
            inventory.assemble(observed, catalog)

    def test_metadata_cannot_guess_source_or_license_information(self):
        for field in ["version", "package", "source", "licenses", "source_delivery"]:
            observed, catalog = fixtures(); del catalog["components"][0][field]
            with self.subTest(field=field), self.assertRaises(inventory.Invalid):
                inventory.assemble(observed, catalog)
        for identity in ["main", "latest", "1.20.0", "sha256:short", "git:" + "g" * 40]:
            observed, catalog = fixtures(); catalog["components"][0]["source"]["identity"] = identity
            with self.subTest(identity=identity), self.assertRaises(inventory.Invalid):
                inventory.assemble(observed, catalog)
        for url in ["file:///private/source", "http://source.example/archive", "https://user:secret@source.example/x",
                    "https://source.example/x?token=secret", "https://source.example/x#secret", "https://bad host/x"]:
            with self.subTest(url=url), self.assertRaises(inventory.Invalid):
                inventory.reference(url)

    def test_schema_types_and_allocation_budgets_fail_closed(self):
        observed, catalog = fixtures()
        for schema in [True, 0, 2, "1", None]:
            observed["schema"] = schema
            with self.subTest(schema=schema), self.assertRaises(inventory.Invalid):
                inventory.assemble(observed, catalog)
        for invalid_size in [True, 0, -1, 4 * 1024**3 + 1, 1.5, "1"]:
            with self.subTest(size=invalid_size), self.assertRaises(inventory.Invalid):
                inventory.size(invalid_size)
        observed, catalog = fixtures()
        with patch.object(inventory, "MAX_FILES", 2), self.assertRaises(inventory.Invalid):
            inventory.assemble(observed, catalog)
        with patch.object(inventory, "MAX_COMPONENTS", 1), self.assertRaises(inventory.Invalid):
            inventory.assemble(observed, catalog)
        for member in observed["native_files"]:
            member["size"] = 2 * 1024**3
        with self.assertRaises(inventory.Invalid):
            inventory.assemble(observed, catalog)

    def test_affected_version_fixture_identifies_only_its_final_files(self):
        observed, catalog = fixtures()
        result = inventory.assemble(observed, catalog)
        affected = {item["id"] for item in result["components"]
                    if item["name"] == "runtime" and item["version"] == "1.20.0"}
        self.assertEqual({item["path"] for item in result["native_files"] if item["component"] in affected},
                         {"bin/core.dll", "libexec/scanner.exe"})
        catalog["components"][0]["version"] = "1.20.1"
        # Rebuild bookkeeping cannot excuse an unrecorded changed payload.
        observed["native_files"][0]["sha256"] = "d" * 64
        with self.assertRaises(inventory.Invalid):
            inventory.assemble(observed, catalog)

    def test_cli_rejects_malformed_json_without_echoing_input(self):
        observed, catalog = fixtures()
        with tempfile.TemporaryDirectory(prefix="balun-inventory-", dir=os.environ.get("TMPDIR") or "/var/tmp") as work:
            observed_path = Path(work) / "observed.json"; catalog_path = Path(work) / "catalog.json"
            observed_path.write_text(json.dumps(observed)); catalog_path.write_text(json.dumps(catalog))
            command = [sys.executable, "-B", str(Path(inventory.__file__)), "--observed", str(observed_path),
                       "--catalog", str(catalog_path), "--format", "cyclonedx"]
            run = subprocess.run(command, text=True, capture_output=True, timeout=10, check=True)
            self.assertEqual(json.loads(run.stdout)["bomFormat"], "CycloneDX")
            for data in [b'{"secret-marker-987":', b'{"schema":1,"schema":1}', b'NaN', b'\xff', b'1' * 5000]:
                observed_path.write_bytes(data)
                run = subprocess.run(command, text=True, capture_output=True, timeout=10, check=False)
                self.assertEqual(run.returncode, 1)
                self.assertEqual(run.stdout, "")
                self.assertNotIn("secret-marker-987", run.stderr)
                self.assertNotIn("Traceback", run.stderr)
            observed_path.write_bytes(b" " * (inventory.MAX_DOCUMENT + 1))
            with self.assertRaises(inventory.Invalid):
                inventory.read_document(observed_path)


if __name__ == "__main__":
    unittest.main()
