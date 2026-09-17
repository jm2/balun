# Settings file trust and responsiveness

Status: maintainer-approved H3.2 contract, September 17, 2026. The maintainer
accepted both the private-profile boundary and the two-second startup/close
limits. The implementation and regressions below are staged in PR #109;
completion takes effect only when CI and bot review are clean and the PR merges.

## Implemented changes

`SettingsStore` pins its profile on first access and shares that handle with its
clones. `cap-std`/`cap-fs-ext` provide relative, no-follow operations without
unsafe code in Balun. The cooperative `.settings.lock` covers admission,
reading, schema validation, and atomic replacement. The actual locked handle
stays open for the entire transaction. Every save re-reads the current document,
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

Reject final-component aliases, non-regular documents, hard-linked documents,
oversized or malformed content, and changed identities during an operation.
Create the Balun directory and temporary files privately. On Unix, enforce the
effective owner and owner-only permissions for the Balun directory/document;
reject a writable-by-others parent. An existing owned profile with read/search
permissions for others, such as the old 0755 directory, is tightened through its
held handle to 0700 only after rejecting foreign ownership and group/other write
permission. Existing documents must already be owner-only; their contents and
permissions are not silently repaired. On Windows, require a normal private profile
with a restrictive inherited DACL and retain that inheritance. Do not claim
that Rust file mode checks attest Windows ACLs.

Use a stable cooperative process lock for read/check/replace transactions.
Re-read the existing document inside every save transaction; preserve an
unsupported schema or malformed document rather than overwriting it. Failed
admission disables persistence for that session, with a fixed value-free error.
The application can continue with in-memory preferences.

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
  directory migration. Windows fixtures require junction refusal and deny
  removal of a held lock. Their results do not attest the inherited DACL.
- `src/ui/settings_session.rs`: blocked startup keeps the GLib context running;
  the late result is discarded; a blocked save retains only the latest queued
  snapshot on one worker; close reaches its deadline, drops queued work, and
  disables later saves; a failed save preserves a newer schema.
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
