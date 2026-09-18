#!/usr/bin/env python3
"""Installed-recipe ownership regressions, independent of current Homebrew data."""

import copy
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import homebrew_metadata as collector
import native_copy_ledger as copies


class HomebrewMetadataTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR") or "/var/tmp")
        self.addCleanup(self.work.cleanup)
        # macOS's temporary root can pass through /var -> /private/var.
        # The collector deliberately queries canonical installed recipe paths.
        self.root = Path(self.work.name).resolve(strict=True)
        self.cellar = self.root / "Cellar"
        self.keg = self.cellar / "fixture" / "1.2.3_2"
        self.recipe = self.keg / ".brew" / "fixture.rb"
        self.recipe.parent.mkdir(parents=True)
        self.recipe.write_text('class Fixture < Formula\n  # exact installed recipe with resources\nend\n')
        self.receipt = self.keg / "INSTALL_RECEIPT.json"
        self.receipt.write_text(json.dumps({"source": {"spec": "stable", "tap": "homebrew/core",
                                                       "versions": {"stable": "1.2.3"}}}))
        self.member = self.keg / "lib" / "fixture.dylib"
        self.member.parent.mkdir()
        self.member.write_bytes(bytes.fromhex("cffaedfe") + b"inert native prefix fixture")
        self.metadata = {"casks": [], "formulae": [{
            "name": "fixture", "versions": {"stable": "1.2.3"}, "revision": 2,
            "ruby_source_checksum": {"sha256": hashlib.sha256(self.recipe.read_bytes()).hexdigest()},
            "urls": {"stable": {"url": "https://example.com/fixture-1.2.3.tar.xz",
                                "checksum": "b" * 64, "revision": None}},
            "license": {"any_of": ["MIT", "Apache-2.0"]}}]}

    def query(self, recipe, _deadline):
        self.assertEqual(recipe, self.recipe)
        return copy.deepcopy(self.metadata)

    def collect(self, query=None, members=None):
        return collector.collect(self.cellar, members or [self.member], query=query or self.query)

    def test_exact_installed_recipe_version_owner_and_source_bytes(self):
        link = self.root / "linked.dylib"
        link.symlink_to(self.member)
        result = self.collect(members=[link])
        package = result["packages"][0]
        self.assertEqual(package["key"], "fixture/1.2.3_2")
        self.assertEqual(package["version"], "1.2.3")
        self.assertEqual(package["license"], {"any_of": ["MIT", "Apache-2.0"]})
        self.assertEqual(package["recipe"]["text"], self.recipe.read_text())
        self.assertEqual(package["primary_source"]["identity"], "sha256:" + "b" * 64)
        self.assertEqual(result["members"], [{"path": "fixture/1.2.3_2/lib/fixture.dylib",
                         "package": "fixture/1.2.3_2", "size": self.member.stat().st_size,
                         "sha256": hashlib.sha256(self.member.read_bytes()).hexdigest()}])
        self.assertNotIn(str(self.root), json.dumps(result))

    def test_current_formula_cannot_replace_installed_recipe(self):
        mutations = [lambda f: f.update(name="other"),
                     lambda f: f.update(versions={"stable": "1.2.4"}),
                     lambda f: f.update(revision=1), lambda f: f.update(revision=True),
                     lambda f: f.update(ruby_source_checksum={"sha256": "c" * 64}),
                     lambda f: f.update(license=None), lambda f: f.update(license={}),
                     lambda f: f.update(urls={"stable": {"url": "https://example.com/main"}})]
        for mutate in mutations:
            with self.subTest(mutate=mutate), self.assertRaises(collector.Invalid):
                def wrong(recipe, deadline):
                    data = self.query(recipe, deadline)
                    mutate(data["formulae"][0])
                    return data
                self.collect(query=wrong)

    def test_git_source_requires_immutable_revision(self):
        stable = self.metadata["formulae"][0]["urls"]["stable"]
        stable.update(checksum=None, revision="d" * 40)
        self.assertEqual(self.collect()["packages"][0]["primary_source"]["identity"], "git:" + "d" * 40)
        for revision in [None, "main", "v1.2.3", "short"]:
            stable["revision"] = revision
            with self.subTest(revision=revision), self.assertRaises(collector.Invalid):
                self.collect()

    def test_receipt_must_identify_the_same_stable_core_package(self):
        for source in [{}, {"spec": "head", "tap": "homebrew/core"},
                       {"spec": "stable", "tap": "some/tap"},
                       {"spec": "stable", "tap": "homebrew/core", "versions": {"stable": "1.2.4"}}]:
            self.receipt.write_text(json.dumps({"source": source}))
            with self.subTest(source=source), self.assertRaises(collector.Invalid):
                self.collect()

    def test_changes_during_query_reject_the_whole_report(self):
        for selected in [self.recipe, self.receipt, self.member]:
            before = selected.read_bytes()
            def changed(recipe, deadline):
                result = self.query(recipe, deadline)
                selected.write_bytes(before + b"changed")
                return result
            with self.subTest(selected=selected.name), self.assertRaises(collector.Invalid):
                self.collect(query=changed)
            selected.write_bytes(before)

    def test_unowned_alias_special_and_duplicate_inputs_fail(self):
        outside = self.root / "outside.dylib"
        outside.write_bytes(self.member.read_bytes())
        with self.assertRaises(collector.Invalid):
            self.collect(members=[outside])
        with self.assertRaises(collector.Invalid):
            self.collect(members=[self.member, self.member])
        target = self.recipe.with_suffix(".target")
        self.recipe.rename(target)
        self.recipe.symlink_to(target)
        with self.assertRaises(collector.Invalid):
            self.collect()
        self.recipe.unlink()
        target.rename(self.recipe)
        self.member.unlink()
        os.mkfifo(self.member)
        with self.assertRaises(collector.Invalid):
            self.collect()

    def test_later_package_cannot_change_an_already_collected_recipe(self):
        other = self.cellar / "second" / "1.2.3_2"
        shutil.copytree(self.keg, other)
        (other / ".brew" / "fixture.rb").rename(other / ".brew" / "second.rb")
        def changed(recipe, deadline):
            if recipe == self.recipe:
                return self.query(recipe, deadline)
            result = copy.deepcopy(self.metadata)
            result["formulae"][0]["name"] = "second"
            self.recipe.write_text(self.recipe.read_text() + "# changed later\n")
            return result
        with self.assertRaises(collector.Invalid):
            self.collect(query=changed, members=[self.member, other / "lib" / "fixture.dylib"])

    def test_budgets_and_malformed_documents(self):
        for name, value in [("MAX_MEMBERS", 0), ("MAX_PACKAGES", 0), ("MAX_TOTAL", 1),
                            ("MAX_METADATA", 4), ("TOTAL_SECONDS", 0), ("MAX_DOCUMENT", 100)]:
            with self.subTest(name=name), patch.object(collector, name, value), self.assertRaises(collector.Invalid):
                self.collect()
        for data in [b'{"a":1,"a":2}', b'NaN', b'\xff', b'{']:
            with self.subTest(data=data), self.assertRaises(ValueError):
                collector.json_document(data)

    def test_real_query_process_is_bounded_and_reaped(self):
        original_popen = subprocess.Popen
        processes = []
        scripts = [('import time; time.sleep(60)', "QUERY_SECONDS", 0.05),
                   ('import sys; sys.stdout.write("x" * 10000)', "MAX_QUERY", 128),
                   ('raise SystemExit(7)', "QUERY_SECONDS", 5)]
        for script, option, limit in scripts:
            def child(args, **kwargs):
                self.assertEqual(args, ["brew", "info", "--json=v2", "--formula", str(self.recipe)])
                self.assertEqual(kwargs["env"]["HOMEBREW_NO_AUTO_UPDATE"], "1")
                process = original_popen([sys.executable, "-B", "-c", script], **kwargs)
                processes.append(process)
                return process
            with self.subTest(option=option), patch.object(collector.subprocess, "Popen", child), \
                    patch.object(collector, option, limit), self.assertRaises(collector.Invalid):
                collector.query_formula(self.recipe, time.monotonic() + 10)
            self.assertIsNotNone(processes[-1].poll())

    def test_cli_rejection_is_fixed_and_has_no_partial_report(self):
        secret = self.root / "private-marker-9321"
        run = subprocess.run([sys.executable, "-B", collector.__file__, "--cellar", str(self.cellar),
                              "--member", str(secret)], capture_output=True, text=True, timeout=10)
        self.assertEqual(run.returncode, 1)
        self.assertEqual(run.stdout, "")
        self.assertNotIn("private-marker-9321", run.stderr)
        self.assertNotIn("Traceback", run.stderr)

    def test_frozen_copy_ledger_selects_owners_and_rejects_changed_installed_bytes(self):
        tree = self.root / "app"
        tree.mkdir()
        destination = tree / "library.dylib"
        shutil.copyfile(self.member, destination)
        path = self.root / "copies.json"
        ledger = copies.read_ledger(path, missing=True)
        deadline = time.monotonic() + 10
        copies.record_copy(ledger, tree, self.member, destination, self.cellar, deadline)
        destination.write_bytes(destination.read_bytes() + b" relocated")
        copies.write_ledger(path, copies.freeze(ledger, tree, deadline))
        self.assertEqual(collector.collect_from_ledger(self.cellar, path, query=self.query), self.collect())
        self.member.write_bytes(self.member.read_bytes() + b" replaced after copying")
        with self.assertRaisesRegex(collector.Invalid, "no longer match"):
            collector.collect_from_ledger(self.cellar, path, query=self.query)


if __name__ == "__main__":
    unittest.main()
