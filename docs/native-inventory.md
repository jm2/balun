# Native runtime inventory

H1.3 requires an inventory tied to the final artifact's actual native members.
`scripts/inventory/native_inventory.py` implements the portable join and report
format. Platform collection, final-artifact integration, and release attachment
are still pending. H1.3 and issue #91 remain open; existing release assets do
not yet carry this inventory or SBOM.

## Input boundary

The tool consumes two bounded JSON documents from the trusted local build
workspace. It does not open an artifact, inspect native code, execute a payload,
extract an archive, resolve a URL, or authenticate source metadata.

The **observed manifest** has schema `1`, a supported platform, an artifact
basename/version/size/SHA-256, and `native_files` containing relative paths,
positive sizes, and SHA-256 values. The platform adapter must enumerate every
final native member, including plugins and scanner/query helpers, after the
existing package gates have succeeded. It must hash the completed artifact
and tie the observation to the tree reopened from that artifact.

The **catalog** has schema `1`, `components`, `external_components`, and
`native_files`. Each catalog member adds its owning component ID to the same
path/size/hash record. These records must come from the staged copy ledger and
its post-relocation content snapshot, before the independent final observation.
Copying the final manifest into a catalog without establishing ownership does
not prove component identity.

Each bundled component supplies:

- A unique ID, component name and actual version.
- Its package name, package version/revision, and package reference.
- A source reference and explicit `sha256:`, `git:`, or `git-sha256:` identity.
- Nonempty named license information and its source references.
- A source-delivery reference and note recording what the package provides.

The collector must obtain these facts from the installed package/recipe used
for the actual copied bytes. A current upstream version or a filename guess is
not a substitute. Missing source or license information blocks the report.
Recording package-supplied license and delivery information does not establish
that a release has satisfied every applicable license obligation.

Externally managed components instead record a name, requirement, provider,
and reference. No installed version, content hash, or bundled member is
invented for a target system dependency. The Linux/Flatpak adapters must keep
distribution or runtime requirements distinct from included native code.

## Validation and outputs

Membership must match exactly. Unknown, omitted, duplicate, case-colliding,
or unowned native members reject; so do changed hashes/sizes and bundled
components with no final member. An external component cannot own a bundled
file. Paths are portable ASCII relative paths without aliases, device names,
or noncanonical segments. References are HTTPS without credentials, queries,
or fragments. Duplicate JSON keys, unknown record fields, unsupported schemas,
non-finite numbers, invalid types, and incomplete metadata fail closed.

Each input is at most 16 MiB. There are at most 65,536 members, 4,096 components
per category, 32 license entries per bundled component, 1,024 path characters,
and 4 GiB of native member bytes. Joining and grouping members is linear in the
record count; deterministic sorting normalizes package-manager output order.
These are document limits, not an archive sandbox or an OS I/O cancellation
claim. Inputs belong in the existing trusted local build workspace.

```bash
python3 -B scripts/inventory/native_inventory.py \
  --observed observed-native.json --catalog component-catalog.json \
  --format inventory > native-inventory.json
python3 -B scripts/inventory/native_inventory.py \
  --observed observed-native.json --catalog component-catalog.json \
  --format cyclonedx > native-runtime.cdx.json
python3 -B scripts/inventory/test_native_inventory.py
```

The CycloneDX 1.6 output includes the final artifact hash, bundled component
metadata, nested file hashes, and explicit externally managed requirements.
It marks the composition incomplete because this native-runtime scope does
not claim a complete Cargo or non-native resource inventory. The format follows
the [official CycloneDX 1.6 schema](https://github.com/CycloneDX/specification/blob/55343ba19dee1785acf1ce9191540d5fd7b590db/schema/bom-1.6.schema.json).
No signing, source-authentication policy, builder attestation, or provenance
statement is generated. H2.1/H2.3 remain paused by maintainer direction.

## Evidence and remaining integration

Synthetic fixtures exercise exact membership, changed payloads, missing and
unknown components, file aliases/collisions, malformed metadata, allocation
budgets, and CLI failures without input echoes. An affected-version fixture
finds precisely that component's runtime library and scanner, then rejects a
changed rebuild payload whose catalog was not refreshed. This is an inventory
query regression, not H1.4's approved advisory response or rebuild exercise.

Remaining H1.3 work must establish real package ownership and source/license
records on macOS and both Windows architectures, emit observations from the
validated final app/installer/archive, describe externally managed Linux and
Flatpak runtimes, attach reports to every release artifact, and exercise
unknown/changed native members in native CI. Only that integrated result can
complete the ledger outcome.
