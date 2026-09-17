# Native runtime inventory

H1.3 requires an inventory tied to the final artifact's actual native members.
`scripts/inventory/native_inventory.py` implements the portable join and report
format. `observe_native.py` independently hashes the native members of an
already reopened, validated package tree and its completed artifact. The macOS
helper records each native copy's input owner and final staged content. Catalog
assembly, final-artifact integration, and release attachment remain pending.
H1.3 and issue #91 remain open; existing release assets do not yet carry this
inventory or SBOM.

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

## Independent observation

The observer consumes a completed artifact file and the payload tree reopened
by the existing trusted packaging gate. It does not extract an archive or prove
that an arbitrary supplied tree came from an arbitrary supplied artifact. That
relationship remains the platform adapter's responsibility. Neither input may
be actively changed during collection. Existing workspace ancestors are trusted;
this is not H2.4's containment boundary for hostile archives or local writers.

```bash
python3 -B scripts/inventory/observe_native.py \
  --tree reopened/Balun.app --artifact dist/Balun.dmg \
  --platform macos-aarch64 --version 0.1.1 > observed-native.json
python3 -B scripts/inventory/test_observe_native.py
```

The observer examines every ordinary file, including hidden members and
extensionless helpers. ELF, PE/DOS and Mach-O header prefixes identify candidate
native members; their structure, architecture, and dependency closure must have
passed the existing native package gates first. This classifier is not a second
executable-format validator. A named DLL, executable, dylib or shared object
without a recognized prefix fails. Script launchers and ordinary resources are
outside this native-runtime inventory.

The observer refuses member aliases, reparse points, hard links, special files,
nonportable or case-colliding names, identity changes at open, and observed
content or membership changes before completion. Opened identities are checked
before reading, after hashing, and against their names; a final complete tree
scan checks directory identities and membership too. It writes a report only
after all observations succeed. CLI rejection text contains no input paths or
file content.

Limits are 65,536 files/directories, 64 directory levels, 1 GiB per member,
4 GiB of native content, 4 GiB for the artifact, and 16 MiB of report JSON.
Five-minute elapsed-time checks run during traversal and chunked hashing. They
cannot interrupt a blocked OS operation; the invoking native job must retain
its process-level timeout. The artifact must be outside its reopened payload.

Fixtures cover all classifier prefixes, extensionless helpers, exact hashes,
independent catalog joins, changed/unknown final members, alias/special-file
refusal, substitution before open, changes during observation, resource budgets,
and fixed CLI rejection without a partial report. CI runs these on Linux,
macOS, and both Windows architectures, including a native Windows junction.

## Evidence and remaining integration

### macOS native copy ledger

The macOS helper invokes `native_copy_ledger.py` immediately after each native
copy, before relocation changes its bytes. This includes the project binary,
GStreamer plugins, scanner/query helpers, pixbuf loaders, and transitive native
libraries. The copied bytes must equal the selected input. Installed inputs
resolve through expected prefix links into their actual Cellar keg; project
ownership requires an explicit selection. Unknown owners reject the build.

After relocation and the existing signing and runtime gates, the helper freezes
`dist/Balun.native-copies.json`. It independently scans the staged app, rejects
unknown or missing native members, and retains both the original input and final
staged size/hash for each destination. This staged snapshot still needs comparison
with the reopened final artifact before it can complete an inventory. It does
not authenticate the installed inputs or change signing policy.

The ledger uses the observer's member, byte, traversal, identity, and time limits.
It lives outside the payload in the trusted build workspace; the helper serializes
updates. An interrupted write invalidates that build's ledger. Later reads fail
closed, and the helper must freeze the complete ledger before artifact upload.
Fixtures exercise changed copies, unknown/missing members, pre/post-relocation
identities, recursive copies, source links, staged aliases, budgets, and CLI errors.

```bash
python3 -B scripts/inventory/test_native_copy_ledger.py
python3 -B scripts/inventory/homebrew_metadata.py --cellar "$(brew --cellar)" \
  --copy-ledger dist/Balun.native-copies.json > macos-native-owners.json
```

The ledger-driven metadata collector checks the installed source bytes still
match those recorded at copy time. Native macOS CI retains the frozen ledger and
metadata for every copied Homebrew input as internal evidence for 14 days.

### Installed Homebrew metadata

`homebrew_metadata.py` collects the installed recipe, declared source identity,
license metadata, receipt hash, and source-file hashes for selected native inputs.
It resolves expected Homebrew prefix/opt links into an explicitly supplied Cellar,
then derives ownership from that installed keg. It invokes `brew info --json=v2`
with the **exact installed `.brew/<name>.rb` path**, requiring the returned recipe
checksum to match its bytes and the recipe/receipt/keg versions to agree. Asking
Homebrew for a formula by name or accepting its current online metadata cannot
substitute for this check.

```bash
python3 -B scripts/inventory/homebrew_metadata.py --cellar "$(brew --cellar)" \
  --member "$(pkg-config --variable=pluginsdir gstreamer-1.0)/libgstgtk4.dylib" \
  > homebrew-native-inputs.json
python3 -B scripts/inventory/test_homebrew_metadata.py
```

This collector supports stable `homebrew/core` installations with a complete
receipt, a SHA-256 or immutable Git revision for the primary source, and declared
license metadata. HEAD, custom taps, mismatched revisions, current-formula
substitution, unknown owners, aliases in metadata, special files, duplicate
members, and observed input changes reject the entire report. Source member
paths are relative to the Cellar; host paths and raw receipts are not exported.
The installed recipe text is retained with its hash, including its declarations
of additional resources and patches. The primary source version is the package's
version: it does not assert that every embedded resource shares that version or
that the declared primary archive alone supplies every source-delivery obligation.

Homebrew evaluates trusted installed Ruby recipes. Automatic updates and analytics
are disabled for the query; this is a build-input tool, not a sandbox for untrusted
recipes. Each query has a 60-second process deadline and 4 MiB output limit. The
collector limits metadata files to 1 MiB, selected inputs to 4,096 members and
256 packages, each member to 1 GiB, total native bytes to 4 GiB, the final report
to 16 MiB, and overall checkpoints to five minutes. OS reads remain subject to
the invoking CI job's timeout. Query processes are killed and reaped on failure.

The implementation follows Homebrew's
[formula JSON and recipe checksum](https://github.com/Homebrew/brew/blob/main/Library/Homebrew/formula.rb)
and [installed receipt](https://github.com/Homebrew/brew/blob/main/Library/Homebrew/tab/tab.rb)
contracts. Linux fixtures exercise stale metadata, installed revision mismatches,
immutable source identities, substitutions, resource bounds and child cleanup.
Native macOS CI also collects real GTK sink, libav plugin, and pixbuf-query inputs,
then the full set selected by the copy ledger. These intermediate records still
need catalog assembly, embedded-resource representation, and final-artifact
integration below before they establish H1.3.

### Final-artifact integration

Synthetic fixtures exercise exact membership, changed payloads, missing and
unknown components, file aliases/collisions, malformed metadata, allocation
budgets, and CLI failures without input echoes. An affected-version fixture
finds precisely that component's runtime library and scanner, then rejects a
changed rebuild payload whose catalog was not refreshed. This is an inventory
query regression, not H1.4's approved advisory response or rebuild exercise.

Remaining H1.3 work must establish real package ownership and source/license
records on macOS and both Windows architectures, invoke the observer from the
validated final app/installer/archive adapters, describe externally managed Linux and
Flatpak runtimes, attach reports to every release artifact, and exercise
unknown/changed native members in native CI. Only that integrated result can
complete the ledger outcome.
