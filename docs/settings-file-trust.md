# Settings file trust and responsiveness

Status: maintainer-approved H3.2 contract, September 17, 2026. The maintainer
accepted both the private-profile boundary and the two-second startup/close
limits. The implementation and regressions below are staged in PR #109;
completion takes effect only when CI and bot review are clean and the PR merges.
The maintainer later amended it for #152: user-private-group directories,
tightening that never adds permissions, and one in-app notice when persistence
is unavailable.

## Implemented changes

`SettingsStore` pins its profile on first access and shares that handle with its
clones. `cap-std`/`cap-fs-ext` provide relative, no-follow operations without
unsafe code in Balun. The cooperative `.settings.lock` covers admission,
reading, schema validation, and atomic replacement. The actual locked handle
stays open for the entire transaction and is explicitly unlocked before closing.
On Unix, a concurrent process spawn can briefly inherit the same open file
description; closing only the parent descriptor would retain the lock until the
child closes its inherited descriptor. A guard releases it on every transaction
exit, including validation failures. Every save re-reads the current document,
so a newer or malformed file introduced after startup is preserved.

The desktop uses one named thread for loading and every subsequent save. One
snapshot can be in flight and at most the newest snapshot is queued. Both
startup loading and close-time draining have a two-second main-context timer.
Timeout or failed admission disables further persistence for that session;
late load results cannot replace in-memory preferences. A stopped worker is
never replaced. Quit during startup cancels loading and joins the controller.

## Accepted trust boundary

Support a local, private, per-user profile. Trust the operating system, the
account owner, and the existing configuration parent selected by that account.
An existing parent may include administrator/user-managed aliases before the
initial directory handle is acquired; those aliases are part of that trusted
configuration. After acquisition, use pinned directory handles and relative
operations for the Balun directory, lock, document, and temporary siblings.
Absolute configuration paths may contain parent components within that existing
parent. The OS resolves them during acquisition, preserving alias semantics;
they are not collapsed lexically by Balun. A missing suffix must contain ordinary
names. On Unix, an unresolved `missing/..` parent fails without creating either
the missing directory or a different profile; Windows retains its native path semantics.

Reject final-component aliases, non-regular documents, hard-linked documents,
oversized or malformed content, and changed identities during an operation.
Create the Balun directory and temporary files privately. On Unix, enforce the
effective owner and owner-only permissions for the Balun directory/document;
reject a parent writable by another account. A world-writable directory is
always rejected. Group write is accepted only on a directory the effective user owns
whose group is that account's user-private group (UPG), as a 002 umask creates:
the directory's group is the effective group, `getgrgid_r` names it exactly as
`getpwuid_r` names the effective user, and it lists no other member. A failed,
empty, or non-UTF-8 lookup rejects; the lookup runs only for group-writable
directories. An existing owned profile with group or other permissions, such as
the old 0755 directory or a UPG 0775 one, has only those bits cleared through its
held handle after admission. Owner bits are never added, so a read-only 0500
profile stays read-only and its saves fail. Existing documents must already be
owner-only; their contents and permissions are not silently repaired. On
Windows, require a normal private profile with a restrictive inherited DACL and
retain that inheritance. Do not claim that Rust file mode checks attest Windows
ACLs.

Use a stable cooperative process lock for read/check/replace transactions.
Re-read the existing document inside every save transaction; preserve an
unsupported schema or malformed document rather than overwriting it. Failed
admission disables persistence for that session, with a fixed value-free error.
The application can continue with in-memory preferences. The window shows one
localized notice per session when no profile, load, or worker is admitted, or a
later save or the worker fails; a normal close shows none. The console keeps the fixed error.

This accepted boundary excludes shared profiles, network filesystems, malicious
same-account processes, and editors that race an active transaction without
honoring the lock. It cannot provide confidentiality against someone who can
read the private profile. Normal pathname replacement must still be detected or
contained by the pinned handles; excluding a hostile account owner is not a
reason to retain the current check/open race.

Exception owner: `jm2`. Revisit before beta, profile import/export/sync,
support for a shared or network profile, or a change to the local-user threat
model. Stronger Windows ACL attestation or hostile-writer transaction semantics
would require a separately reviewed platform design.

## Retired routed approval store (2026-09-24)

`settings.json` never held route-derived approvals, so retiring that discovery
([ADR-0003](architecture/adr-0003-retire-route-derived-discovery.md)) changes
neither its schema nor its loading: v0.1.x settings files load unchanged. v0.1.x
kept its approvals in a separate `routed-approvals/` directory beside
`settings.json` (`routed-approvals.json`, `routed-approvals.key`, and
`routed-approvals.lock`). Balun now leaves that directory untouched: it never
reads, writes, or deletes it. A downgrade to v0.1.x therefore finds its
approvals intact, and no deletion path is added to the private profile. Users
may delete the directory by hand; it holds only keyed fingerprints and bounded
policy state, never raw routes, interface names, or prefixes.

## Remembered subnet (V2.4, 2026-09-24)

Subnet search remembers only the last subnet the user entered, as `subnet_prefix`
in schema 3. It is editable text for the next search and never authorization:
every search still needs a fresh confirmation, and a failed save cannot skip one.
A document is written as schema 3 only while it holds a prefix, and as schema 2
otherwise, so **Forget subnet** restores a file earlier builds read. Earlier
builds report a schema-3 file as unsupported and leave it untouched. A prefix
that is not a canonical private `/23`–`/32` subnet makes the document malformed,
and it is preserved like any other.

## Accepted responsiveness and durability tradeoff

Keep filesystem work off the GTK main context. Use one bounded worker per
settings session and retain at most the newest queued save. A stuck operation
must not cause a replacement thread or an unbounded work queue.

Allow at most two seconds for startup settings loading. On timeout, show the
window with defaults and disable persistence for that session; discard a late
load result. Allow at most two seconds for close-time draining, then close even
if the last preference update could not be saved. Drop queued work and prevent
publication after cancellation where the worker has not entered the atomic
publication operation. OS I/O already in progress is not claimed cancellable.

The tradeoff is explicit: startup and close remain responsive, but a slow or
failed disk can lose the newest preferences. Flush a complete temporary sibling
before publication so this does not turn a valid document into partial JSON.
An atomic publication already in the OS may finish after the deadline; the
application must not claim that a timed-out save definitely did not occur.

## Regression evidence and limits

The portable store tests run with the library on Linux, macOS, and Windows;
the desktop worker tests run in the native desktop jobs. Local Linux results
are recorded in the PR; native CI must pass before merge.

- `src/settings/store.rs`: forced admission/open substitution with a regular
  file, outside symlink, hard link, and Unix FIFO; hard-linked lock rejection;
  independent cooperative writers; newer/malformed schema preservation;
  profile rename containment; Unix lock loss and mode/owner rejection; private
  directory migration; user-private-group admission with other-group,
  failed-lookup, world-writable, and foreign-owner refusal; a read-only profile
  that loses only group/other bits. Windows fixtures require junction refusal
  and deny removal of a held lock. Their results do not attest the inherited DACL.
- `src/ui/settings_session.rs`: blocked startup keeps the GLib context running;
  the late result is discarded; a blocked save retains only the latest queued
  snapshot on one worker; close reaches its deadline, drops queued work, and
  disables later saves; a failed save preserves a newer schema. A failed or
  missing load, a failed save, and a worker panic each offer one notice; a
  normal close offers none.
- Store fixtures pause at the write and flush boundaries, then cancel and
  resume. They verify that the prior complete document remains and temporary
  siblings are cleaned. These are deterministic boundary injections, not proof
  of interrupting a kernel read, write, flush, or rename that never returns.
- `scripts/test-desktop-lifecycle.sh`: real GTK startup, repeated activation,
  quit before window creation, normal close, and About/quit join the controller.
  The native playback hang limitation remains the accepted H3.5 boundary.
- The Linux coverage ratchet includes schema admission, profile transactions,
  settings-session decisions, and the single worker. Platform-only paths still
  require their native regression jobs.

## Required acceptance evidence

- Force replacement between admission and open; reject a substituted symlink,
  special file, or hard link without reading outside the admitted document.
- Replace or rename the profile path after pinning; operations must remain on
  the admitted directory or fail, never follow its replacement.
- Introduce a newer-schema or malformed document after startup; saving must
  preserve its bytes. Exercise cooperative simultaneous writers and lock loss.
- Verify Unix ownership/mode rejection and native Windows reparse/sharing
  behavior. Record the inherited-DACL assumption without reporting it as an ACL
  test result. Run the portable transactions on all three supported platforms.
- Inject a blocked load, write, and flush in isolated fixtures. Prove the UI
  deadline, one-worker limit, bounded queue, late-result rejection, and complete
  document behavior without claiming to cancel an arbitrary OS syscall.

H3.2 completes only when the accepted contract, implementation, regressions,
documentation, CI, and bot review land together. A checked PR ledger is staged
for that merge, not a claim that a still-open PR has already landed.
