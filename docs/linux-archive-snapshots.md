# Linux archive input snapshots

H2.4 input-consistency slice, September 17, 2026. Debian, RPM, and Arch package
inspection now copies the completed local artifact once into its private scratch
directory. Every metadata query and payload extraction reads that same snapshot.
Replacing the original package between tools cannot combine metadata from one
version with payload from another.

## Snapshot contract

`build-aux/linux/archive_snapshot.py` admits one ordinary, single-link input file
of at most 1 GiB. It rejects a leaf symlink, hard-linked file, directory, FIFO,
or other special file before copying. A nonblocking, no-follow open must match
the initial path metadata. Copying uses 64 KiB chunks, checks both the declared
size and cumulative byte cap, and requires matching descriptor/path identity,
mode, link count, size, modification time, and change time afterward.

The output is created exclusively with mode `0600` inside the validation job's
private directory. Existing files and aliases are never overwritten. Failed
copies remove their partial output; the surrounding validator removes the whole
scratch directory on success or failure. A successful snapshot remains usable
even if the original artifact changes afterward. No native package inspector
runs before snapshot creation succeeds.

The copy checks a 30-second elapsed-time budget between synchronous filesystem
operations. This cannot interrupt an OS read/write/close stall. It bounds neither
the subsequent native parser nor decompression/extraction work. Input paths may
have trusted existing parent directories; the selected archive leaf must be
ordinary. The destination parent is private to one serialized local validation
job. Hostile same-account writers, privileged metadata forgery, and concurrent
mutation of that private scratch directory are outside this contract.

The archive format is still checked by its existing native tools. The snapshot
does not interpret or execute its content. Native tools identify the format from
the private file's bytes; its temporary name has no package extension. Metadata,
component, native-import, and final tree checks still run on the completed
artifact as before. Python 3 is now an explicit prerequisite of the three Linux
native packaging modes, checked before Cargo or a native packager runs.

## Validation and remaining boundary

Unit tests force source replacement between stat/open and during copying,
in-place edits, growth/truncation, alias/special-file inputs, size/time limits,
short/failed writes, existing outputs, private permissions, and fixed CLI errors.
Native-tool routing fixtures replace the original input after the first query
and require every later Debian/RPM/Arch read to retain one unchanged private
path. They also prove oversized archives reach no native tool and that snapshot
cleanup runs. The source-policy classifier has a negative fixture for the new
Python helper, and build routing checks Python availability before build work.

```bash
export TMPDIR="${TMPDIR:-/var/tmp}"
python3 -B build-aux/linux/test_archive_snapshot.py
build-aux/linux/test-package-compliance.sh
scripts/test-build-linux-policy.sh
build-aux/packaging/test-release-component-policy.sh
```

H2.4 remains unchecked. Native archive-member path/type/link preflight,
decompression and extraction budgets, extractor containment, and malicious
archive fixtures are still required before accepting artifacts from outside
the trusted local build boundary. The 1 GiB input cap does not bound expanded
payload size. Existing post-extraction tree limits cannot prevent an unsafe
extractor from writing elsewhere first. This change does not alter Flatpak,
Windows, or DMG inspection or establish signing/provenance guarantees.
