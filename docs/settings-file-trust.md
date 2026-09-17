# Settings file trust and responsiveness

Status: maintainer-approved H3.2 contract, September 17, 2026. The maintainer
accepted both the private-profile boundary and the two-second startup/close
limits. Implementation is in progress; this document alone does not complete
H3.2 or claim the implementation already enforces it.

## Current gaps

`SettingsStore::load` checks the pathname and then opens it normally. Replacement
between those operations can bypass the final-component symlink check. Parent
directory traversal is not pinned. Saves create parent directories recursively
and replace the document without checking whether a newer schema appeared after
startup. The desktop loads synchronously and waits indefinitely for saves when
closing. File-size limits do not bound a stalled filesystem operation.

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
reject a writable-by-others parent. On Windows, require a normal private profile
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

## Required implementation evidence

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

H3.2 remains unchecked until the accepted contract, implementation, regression
tests, documentation, CI, and bot review have landed.
