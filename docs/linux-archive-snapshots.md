# Linux archive input snapshots and RPM preflight

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

## RPM payload preflight

The RPM gate now runs `rpm2cpio` as one owned process group and captures at most
1 GiB of expanded output in a new private `0600` file. A 60-second elapsed-time
budget covers producer output, producer exit, and preflight. Failure kills and
reaps the producer, kills descendants remaining in its process group, and
removes the partial output. The leader remains waitable until group cleanup, so
its process ID cannot be reused before cleanup signals the group. Synchronous
filesystem stalls and uninterruptible kernel waits remain outside this timeout.

Before `cpio` runs, `build-aux/linux/rpm_payload.py` validates the whole decoded
`newc` or CRC archive. It admits regular files, directories, and confined relative
symlinks pointing directly to included regular files, including RPM build-ID
links. It rejects special files, hard links, privileged or unknown mode bits,
absolute/traversing/ambiguous member paths, duplicate names, file or symlink
ancestors, link chains, directory links, and dangling links. Header lengths,
hex fields, NUL termination, padding, CRC checksums, and the final trailer are
checked before any member is extracted. Paths and link text must be printable
ASCII without backslashes; an optional single leading `./` on a member is allowed.

The limits are 8,192 entries including implicit parent directories, 64 directory
levels, 2,048 bytes per path or link target, 256 MiB per file, and 1 GiB for the
entire decoded archive including headers. At most 64 KiB of zero padding may
follow the trailer. These bounds constrain preflight's storage and traversal as
well as the admitted tree. The existing native extractor then runs without
preserving archived ownership, and the final tree/component checks still apply.

The decoded archive and its parent stay private to the same serialized local
validation job. No hostile same-account writer or concurrent mutation is
admitted. This is a format-specific correctness gate, not an RPM parser sandbox
or a proof that arbitrary hostile input is safe for native code.

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
python3 -B build-aux/linux/test_rpm_payload.py
```

RPM tests cover malformed members, parent/link interpretation in either member
order, implicit-directory budgets, CRC and truncation, producer failure/stall,
private output, and partial cleanup. Integration fixtures prove invalid or
oversized declared members never invoke `cpio`. CI installs RPM/CPIO tools and
builds an inert real RPM, preflights and extracts it, verifies its data and
relative build-ID-style link, and runs the complete package validator.

H2.4 remains unchecked. RPM header queries and native extraction still need
time/output containment; the decoder itself is native code without an OS sandbox.
Debian and Arch still lack member preflight and expanded-payload budgets.
Extractor isolation and broader format-specific negative fixtures remain
required before accepting artifacts outside the trusted local build boundary.
Existing post-extraction checks cannot contain a compromised native parser.
This change does not alter Flatpak, Windows, or DMG inspection or establish
signing/provenance guarantees.
