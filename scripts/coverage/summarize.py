#!/usr/bin/env python3
"""Report scoped LLVM regions, Python branches, and PowerShell lines separately."""

import argparse
import json
from pathlib import Path
import re
import subprocess
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
SCOPE = json.loads((Path(__file__).with_name("scope.json")).read_text())


def metric(covered, total, missing):
    if not total or not 0 <= covered <= total:
        raise ValueError("missing or invalid coverage denominator")
    return {"covered": covered, "total": total, "uncovered": total - covered,
            "percent": round(covered * 100 / total, 2), "missing": missing}


def rust_metrics(path):
    document = json.loads(path.read_text())
    functions = []
    for unit in document["data"]:
        for function in unit["functions"]:
            if any(Path(name).is_relative_to(ROOT) and
                   str(Path(name).relative_to(ROOT)) in SCOPE["rust"]
                   for name in function["filenames"]):
                functions.append(function)
    demangled = subprocess.run(
        ["c++filt", "--format=rust"], text=True, capture_output=True, check=True,
        input="\n".join(function["name"] for function in functions) + "\n",
    ).stdout.splitlines()
    if len(demangled) != len(functions) or any(name.startswith("_R") for name in demangled):
        raise ValueError("Rust symbol demangling failed; cannot exclude test functions")
    excluded = re.compile("|".join(SCOPE["rust_excluded_names"]))
    by_file = {name: {} for name in SCOPE["rust"]}
    for function, name in zip(functions, demangled):
        if excluded.search(name):
            continue
        for region in function["regions"]:
            source = Path(function["filenames"][region[5]])
            if region[7] != 0 or not source.is_relative_to(ROOT):
                continue
            relative = str(source.relative_to(ROOT))
            if relative in by_file:
                # The same source region may appear in several test binaries or
                # generic instantiations. Count it once, covered if any ran it.
                key = tuple(region[:4])
                by_file[relative][key] = by_file[relative].get(key, False) or region[4] > 0
    return {f"rust-regions:{name}": metric(sum(regions.values()), len(regions),
            [list(region) for region, hit in sorted(regions.items()) if not hit])
            for name, regions in by_file.items()}


def python_metrics(path):
    files = json.loads(path.read_text())["files"]
    result = {}
    for name in SCOPE["python"]:
        coverage = files[name]
        summary = coverage["summary"]
        result[f"python-lines:{name}"] = metric(
            summary["covered_lines"], summary["num_statements"], coverage["missing_lines"])
        result[f"python-branches:{name}"] = metric(
            summary["covered_branches"], summary["num_branches"], coverage["missing_branches"])
    return result


def powershell_metrics(path):
    classes = {node.get("filename").replace("\\", "/"): node
               for node in ET.parse(path).getroot().findall(".//class")}
    result = {}
    for name, selected in SCOPE["powershell"].items():
        methods = {node.get("name"): node for node in classes[name].findall("./methods/method")}
        for function in selected:
            # Pester's Cobertura branch-rate is not branch measurement. Only
            # report actual source-line hits for explicitly listed gate functions.
            lines = {int(line.get("number")): int(line.get("hits")) > 0
                     for line in methods[function].findall("./lines/line")}
            result[f"powershell-lines:{name}:{function}"] = metric(
                sum(lines.values()), len(lines), [line for line, hit in sorted(lines.items()) if not hit])
    return result


def regressions(result, baseline):
    if result.keys() != baseline.keys():
        raise ValueError("coverage scope changed without an explicit baseline review")
    return [name for name, before in baseline.items()
            if result[name]["uncovered"] > before["uncovered"]
            or result[name]["total"] < before["total"]]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path)
    parser.add_argument("--python", type=Path)
    parser.add_argument("--powershell", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--baseline", type=Path)
    args = parser.parse_args()
    result = {}
    prefixes = []
    for path, collect, prefix in ((args.rust, rust_metrics, "rust-"),
                                  (args.python, python_metrics, "python-"),
                                  (args.powershell, powershell_metrics, "powershell-")):
        if path:
            result.update(collect(path))
            prefixes.append(prefix)
    if not result:
        parser.error("at least one coverage input is required")
    args.output.write_text(json.dumps({"schema": 1, "metrics": result}, indent=2) + "\n")
    failures = []
    if args.baseline:
        baseline = {name: value for name, value in
                    json.loads(args.baseline.read_text())["metrics"].items()
                    if name.startswith(tuple(prefixes))}
        # No silent deletion of instrumented code, or increase in uncovered
        # regions/branches/lines. Legitimate scope changes require review.
        failures = regressions(result, baseline)
    for name, item in result.items():
        print(f'{name}: {item["covered"]}/{item["total"]} ({item["percent"]:.2f}%)')
    if failures:
        raise SystemExit("Coverage ratchet regressed: " + ", ".join(failures))


if __name__ == "__main__":
    main()
