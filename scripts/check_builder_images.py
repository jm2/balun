#!/usr/bin/env python3
"""Compare every job container with the reviewed image inventory (requires PyYAML)."""

import argparse
import json
from pathlib import Path
import re
import sys

import yaml

POLICY = Path("build-aux/toolchain/builder-images.json")
WORKFLOWS = Path(".github/workflows")
IMAGE = re.compile(r"[a-z0-9][a-z0-9./:_-]*@sha256:[0-9a-f]{64}")


class Invalid(ValueError):
    """The checked-in workflow and input policy disagree."""


def require(condition):
    if not condition:
        raise Invalid("builder image policy mismatch")


def unique_mapping(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result)
        result[key] = value
    return result


class UniqueLoader(yaml.SafeLoader):
    """Reject ambiguous duplicate keys rather than keeping their last value."""


def yaml_mapping(loader, node):
    loader.flatten_mapping(node)
    return unique_mapping(loader.construct_pairs(node))


UniqueLoader.add_constructor(yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, yaml_mapping)


def read_policy(root):
    policy = json.loads((root / POLICY).read_text(), object_pairs_hook=unique_mapping)
    require(isinstance(policy, dict) and set(policy) == {"schema", "workflows"})
    require(type(policy["schema"]) is int and policy["schema"] == 1)
    expected = policy["workflows"]
    require(isinstance(expected, dict) and bool(expected))
    for filename, jobs in expected.items():
        require(re.fullmatch(r"[A-Za-z0-9_-]+\.ya?ml", filename) is not None)
        require(isinstance(jobs, dict) and bool(jobs))
        for job, image in jobs.items():
            require(isinstance(job, str) and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_-]*", job) is not None)
            require(isinstance(image, str) and IMAGE.fullmatch(image) is not None)
    return expected


def check(root):
    expected = read_policy(root)
    observed = {}
    for path in sorted((root / WORKFLOWS).iterdir()):
        if path.suffix not in {".yml", ".yaml"}:
            continue
        workflow = yaml.load(path.read_text(), Loader=UniqueLoader)
        require(isinstance(workflow, dict) and isinstance(workflow.get("jobs"), dict))
        containers = {}
        for job, definition in workflow["jobs"].items():
            require(isinstance(definition, dict))
            if "container" not in definition:
                continue
            container = definition["container"]
            image = container.get("image") if isinstance(container, dict) else container
            require(isinstance(image, str) and IMAGE.fullmatch(image) is not None)
            containers[job] = image
        if containers:
            observed[path.name] = containers
    require(observed == expected)
    return sum(len(jobs) for jobs in observed.values())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    args = parser.parse_args()
    try:
        count = check(args.root)
    except (OSError, ValueError, TypeError, RecursionError, yaml.YAMLError):
        print("Builder image policy rejected: update reviewed pins and workflows together", file=sys.stderr)
        return 1
    print(f"Reviewed builder image pins match all {count} job containers")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
