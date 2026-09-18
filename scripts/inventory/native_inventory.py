#!/usr/bin/env python3
"""Join package metadata to an independently observed final native-file manifest.

Inputs are bounded local build records, not native artifacts to load or extract.
No signatures, builder attestations, or source-authentication claims are produced.
"""

import argparse
import json
from pathlib import Path
import re
import sys
from urllib.parse import urlsplit

MAX_DOCUMENT = 16 * 1024 * 1024
MAX_FILES = 65536
MAX_COMPONENTS = 4096
PLATFORMS = {"macos-aarch64", "windows-x86_64", "windows-aarch64",
             "linux-x86_64", "linux-aarch64"}


class Invalid(ValueError):
    """A closed input contract was not satisfied."""


def require(condition, reason):
    if not condition:
        raise Invalid(reason)


def fields(value, names):
    require(isinstance(value, dict) and set(value) == set(names.split()),
            "record fields do not match schema")


def text(value, limit=512):
    require(isinstance(value, str) and 0 < len(value) <= limit
            and value == value.strip() and value.isprintable(),
            "invalid bounded text")
    return value


def digest(value):
    require(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value),
            "invalid SHA-256")
    return value


def size(value):
    require(type(value) is int and 0 < value <= 4 * 1024**3, "invalid content size")
    return value


def member_path(value):
    text(value, 1024)
    require(not value.startswith("/") and "\\" not in value and ":" not in value,
            "member path is not relative and portable")
    require(all(32 <= ord(char) < 127 and char not in '<>"|?*' for char in value),
            "member path contains non-portable characters")
    require(all(part not in {"", ".", ".."} and not part.endswith((" ", "."))
                for part in value.split("/")), "member path is not canonical")
    require(not any(re.fullmatch(r"(?i:con|prn|aux|nul|com[1-9]|lpt[1-9])", part.split(".")[0])
                    for part in value.split("/")), "member path uses a reserved device name")
    return value


def reference(value):
    text(value, 2048)
    try:
        parsed = urlsplit(value)
        valid = (parsed.scheme == "https" and parsed.hostname and parsed.username is None
                 and parsed.password is None and parsed.port in (None, 443)
                 and not parsed.query and not parsed.fragment and "\\" not in value
                 and not any(char.isspace() for char in value))
    except ValueError:
        valid = False
    require(valid, "reference must be an HTTPS URL without credentials, query or fragment")
    return value


def identifier(value):
    require(isinstance(value, str) and re.fullmatch(r"[a-z0-9][a-z0-9._+-]{0,127}", value),
            "invalid component identifier")
    return value


def records(value, limit, *, allow_empty=False):
    require(isinstance(value, list) and (allow_empty or value) and len(value) <= limit,
            "record count is empty or exceeds its budget")
    return value


def file_records(value, *, owners=False):
    result = {}
    folded = set()
    total = 0
    for item in records(value, MAX_FILES):
        fields(item, "path sha256 size component" if owners else "path sha256 size")
        path = member_path(item["path"])
        require(path.casefold() not in folded, "duplicate or case-colliding native member")
        folded.add(path.casefold())
        digest(item["sha256"])
        total += size(item["size"])
        require(total <= 4 * 1024**3, "native payload exceeds its size budget")
        if owners:
            identifier(item["component"])
        result[path] = dict(item)
    return result


def bundled_components(value):
    result = {}
    for item in records(value, MAX_COMPONENTS):
        fields(item, "id name version package source licenses source_delivery")
        key = identifier(item["id"])
        require(key not in result, "duplicate component identifier")
        text(item["name"])
        text(item["version"])
        fields(item["package"], "name version reference")
        text(item["package"]["name"])
        text(item["package"]["version"])
        reference(item["package"]["reference"])
        fields(item["source"], "reference identity")
        reference(item["source"]["reference"])
        # Identity is an explicit archive/recipe digest or immutable revision,
        # never inferred from a filename or a current upstream version.
        identity = text(item["source"]["identity"])
        require(re.fullmatch(r"(?:sha256:[0-9a-f]{64}|git:[0-9a-f]{40}|git-sha256:[0-9a-f]{64})", identity),
                "source identity is not an explicit digest or immutable revision")
        for license_item in records(item["licenses"], 32):
            fields(license_item, "name reference")
            text(license_item["name"])
            reference(license_item["reference"])
        fields(item["source_delivery"], "reference note")
        reference(item["source_delivery"]["reference"])
        text(item["source_delivery"]["note"], 2048)
        result[key] = item
    return result


def external_components(value, bundled):
    result = {}
    for item in records(value, MAX_COMPONENTS, allow_empty=True):
        fields(item, "id name requirement provider reference")
        key = identifier(item["id"])
        require(key not in result and key not in bundled, "duplicate component identifier")
        for name in ("name", "requirement", "provider"):
            text(item[name])
        reference(item["reference"])
        result[key] = item
    return result


def assemble(observed, catalog):
    fields(observed, "schema platform artifact native_files")
    fields(catalog, "schema components external_components native_files")
    require(type(observed["schema"]) is int and observed["schema"] == 1
            and type(catalog["schema"]) is int and catalog["schema"] == 1,
            "unsupported inventory schema")
    require(isinstance(observed["platform"], str) and observed["platform"] in PLATFORMS,
            "unsupported target platform")
    artifact = observed["artifact"]
    fields(artifact, "name version sha256 size")
    require("/" not in member_path(artifact["name"]), "artifact name must be a basename")
    text(artifact["version"])
    digest(artifact["sha256"])
    size(artifact["size"])
    actual = file_records(observed["native_files"])
    declared = file_records(catalog["native_files"], owners=True)
    components = bundled_components(catalog["components"])
    external = external_components(catalog["external_components"], components)
    require(actual.keys() == declared.keys(), "unknown or missing final native member")
    used = set()
    for path, member in actual.items():
        owned = declared[path]
        require(member["sha256"] == owned["sha256"] and member["size"] == owned["size"],
                "final native content differs from catalog")
        require(owned["component"] in components, "native member has no bundled component")
        used.add(owned["component"])
    require(used == components.keys(), "bundled component has no final native member")
    return {"schema": 1, "platform": observed["platform"], "artifact": dict(artifact),
            "components": [components[key] for key in sorted(components)],
            "external_components": [external[key] for key in sorted(external)],
            "native_files": [declared[path] for path in sorted(declared)]}


def sha256_hash(value):
    return [{"alg": "SHA-256", "content": value}]


def properties(**items):
    return [{"name": "balun:" + name.replace("_", "-"), "value": str(value)}
            for name, value in items.items()]


def sbom(inventory):
    """CycloneDX 1.6 native inventory; intentionally not a complete Cargo SBOM."""
    components = []
    by_owner = {item["id"]: [] for item in inventory["components"]}
    for member in inventory["native_files"]:
        by_owner[member["component"]].append(member)
    for item in inventory["components"]:
        files = [{"type": "file", "bom-ref": "file:" + member["path"],
                  "name": member["path"], "hashes": sha256_hash(member["sha256"]),
                  "properties": properties(size=member["size"])}
                 for member in by_owner[item["id"]]]
        components.append({
            "type": "library", "bom-ref": "component:" + item["id"],
            "name": item["name"], "version": item["version"], "scope": "required",
            "licenses": [{"license": {"name": value["name"], "url": value["reference"]}}
                         for value in item["licenses"]],
            "externalReferences": [
                {"type": "distribution", "url": item["package"]["reference"]},
                {"type": "source-distribution", "url": item["source_delivery"]["reference"],
                 "comment": item["source_delivery"]["note"]},
                {"type": "other", "url": item["source"]["reference"],
                 "comment": "Source identity: " + item["source"]["identity"]}],
            "properties": properties(management="bundled", package_name=item["package"]["name"],
                                     package_version=item["package"]["version"],
                                     source_identity=item["source"]["identity"]),
            "components": files,
        })
    for item in inventory["external_components"]:
        components.append({
            "type": "library", "bom-ref": "component:" + item["id"], "name": item["name"],
            "scope": "required", "description": item["requirement"],
            "externalReferences": [{"type": "other", "url": item["reference"]}],
            "properties": properties(management="externally-managed", provider=item["provider"]),
        })
    artifact = inventory["artifact"]
    return {
        "$schema": "http://cyclonedx.org/schema/bom-1.6.schema.json",
        "bomFormat": "CycloneDX", "specVersion": "1.6", "version": 1,
        "metadata": {"component": {
            "type": "application", "bom-ref": "artifact", "name": "Balun",
            "version": artifact["version"], "hashes": sha256_hash(artifact["sha256"]),
            "properties": properties(artifact=artifact["name"], size=artifact["size"],
                                     platform=inventory["platform"], inventory_scope="native-runtime")}},
        "components": components,
        "dependencies": [{"ref": "artifact", "dependsOn": [item["bom-ref"] for item in components]}],
        "compositions": [{"aggregate": "incomplete", "assemblies": ["artifact"]}],
    }


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON field")
        result[key] = value
    return result


def read_document(path):
    # These are metadata documents in the trusted local build workspace. No
    # native loader or archive extractor is called, and references are not fetched.
    with path.open("rb") as stream:
        data = stream.read(MAX_DOCUMENT + 1)
    require(len(data) <= MAX_DOCUMENT, "inventory document exceeds byte budget")
    return json.loads(data.decode("utf-8"), object_pairs_hook=unique_object,
                      parse_constant=lambda _: (_ for _ in ()).throw(Invalid("non-finite JSON number")))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--observed", type=Path, required=True)
    parser.add_argument("--catalog", type=Path, required=True)
    parser.add_argument("--format", choices=("inventory", "cyclonedx"), default="inventory")
    args = parser.parse_args()
    try:
        inventory = assemble(read_document(args.observed), read_document(args.catalog))
        output = sbom(inventory) if args.format == "cyclonedx" else inventory
        sys.stdout.write(json.dumps(output, sort_keys=True, indent=2, ensure_ascii=True) + "\n")
    except (ValueError, OSError, RecursionError) as error:
        reason = str(error) if isinstance(error, Invalid) else type(error).__name__
        print("Native inventory rejected: " + reason, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
