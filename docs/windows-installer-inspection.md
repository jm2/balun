# Completed Windows installer inspection

H1.1 / [issue #90](https://github.com/jm2/balun/issues/90) adds payload inspection
after Inno Setup compilation. A valid executable/version resource alone does
not prove that the installer contains the staged runtime.

## Gate

The build helper requires `BALUN_INNOEXTRACT_DIR` to name a directory containing
exactly the pinned `innoextract.exe` and `libbz2-1.dll`. It checks both hashes
and rejects aliases, extra tool files, or changed bytes. The build-only tool
is not included in Balun's package. Its installer input is never executed.

Before compiling, the helper captures the complete staging manifest already
bound to the successful runtime probe. It then:

1. Reopens the compiled installer for its existing identity/version checks.
2. Holds the installer against Windows writes, deletion, and replacement, and
   checks its hash before listing and again after all validation.
3. Lists raw member names, sizes, and SHA-256 hashes without extraction. Every
   member must be under the literal `{app}` destination. The preflight refuses
   inspector warnings, unknown records/checksums, absolute/escaping paths, controls, non-ASCII names,
   alternate streams, reserved Windows names, ambiguous trailing dots/spaces,
   duplicate or case-colliding paths, and file/directory conflicts.
4. Compares the complete manifest, including implied and empty directories,
   with the validated staging manifest before allowing extraction.
5. Extracts into a fresh temporary directory with filename sanitization and
   collision refusal enabled. Only an `app` tree may appear. Re-enumeration
   refuses reparse points, hard-link aliases, alternate streams, and unsafe
   members; all paths, types, sizes, and hashes must match staging exactly.
6. Repeats native architecture/import/component and application-resource
   checks on the extracted tree, then runs Balun's existing relocated package
   runtime probe against that identical payload. The final tree, installer,
   and original staging identity are rechecked before the helper succeeds.

Input is limited to 1 GiB; each file to 1 GiB; the expanded payload to 4 GiB;
membership to 65,536 entries; paths to 64 components and 1,024 characters;
and listing/manifest text to 16 MiB. Inspector processes have five-minute
deadlines and a polled 16 MiB combined-output limit. Scratch is deleted on exit.
These limits retain the existing trusted-local-build-output boundary: they
are not a sandbox for a compromised native parser. H2.4 remains separate.

## Tool inputs

`build-aux/inno/install-inspector.ps1` downloads the fixed
[Windows extractor release 670](https://github.com/UserUnknownFactor/innoextract_win/releases/tag/670).
The archive and both members are hash-pinned in
`scripts/windows-installer-policy.ps1`. Downloading is bounded to 2 MiB and
60 seconds; ZIP members are checked against the exact expected names, types,
sizes, and hashes before writing. The ordinary build helper never downloads
or updates a tool. CI/release install the tested Inno Setup 6.7.3 compiler.

Upstream [innoextract](https://github.com/dscharrer/innoextract) advertises support
through Inno Setup 6.3.3. The chosen Windows fork supplies 6.7 support. The
[upstream 6.7 proposal](https://github.com/dscharrer/innoextract/pull/205) at
`e8960a05eef5ea218bf05a1e23adc3f688c3d5b5` was built separately on Linux for
the compatibility experiment below. It is not substituted for the pinned
Windows executable. The Windows tool is x64 and uses Windows' x64 application
support on the ARM64 builder; the inspected payload architecture is checked
independently. Updating any of these inputs requires a reviewed change and
both native CI lanes. This is local input pinning, not release provenance.

For a local Windows build, use PowerShell 7.2 or newer to install the tool into
a new directory explicitly:

```powershell
./build-aux/inno/install-inspector.ps1 -Destination C:\BuildTools\BalunInnoInspector
$env:BALUN_INNOEXTRACT_DIR = 'C:\BuildTools\BalunInnoInspector'
./scripts/build-windows.ps1 -InnoSetup
```

## Evidence and limits

Portable tests execute the production manifest parser and inspection gate.
Changed, missing, extra, escaping, ambiguous, oversized, aliased-type, and
colliding declarations must fail before the controlled extractor can run.
The tests check empty-directory identity, wrong PE architecture, required
checks on the extracted tree, and mutation during final runtime validation.
The existing full-tree receipt tests still pass for both architecture profiles.

The September 17 compatibility experiment verified the published v0.1.0
installers and ZIPs against their release `SHA256SUMS.txt`, preflighted both
installer listings, and extracted without running either installer. All
1,097 x86_64 files and 1,103 ARM64 files matched their corresponding ZIPs in
size and SHA-256. The production PowerShell parser/inspector wrapper also
matched every listed/extracted file and directory for both samples using
the separately built Linux inspector. This is static historical evidence,
not a claim that the new Windows implementation has passed native CI.

Both Windows CI lanes now compile the current installer, perform the complete
inspection and runtime-probe sequence, and upload the ZIP and installer only
after it succeeds. Those checks and clean bot review are required before this
change merges. Running the installer and validating installed live playback
remain in P4.1; copying bytes correctly does not establish installed behavior.
