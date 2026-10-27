# Nemo-style workflows and continued fixes

This follow-up extends the file manager's everyday workflows without claiming
complete Nemo compatibility.

## Available actions

- **Create New Document → Empty Document** in the File menu or a folder's
  background menu creates an empty file and selects its name for inline editing. Local and live Gungnir paths support exclusive creation; an
  existing file or symlink is never overwritten. Other backends return an
  explicit unsupported-operation error.
- **Create New Folder** works the same way from the background or File menu, or
  with `Ctrl+Shift+N` / `F7`. Existing default names get numeric suffixes. Inline
  naming works in detailed, compact, and grid views; Escape keeps the new item.
- **Select Items Matching** (`Ctrl+S`) selects visible names matching a
  case-sensitive pattern. `*` matches any sequence and `?` one Unicode character.
  Hidden or filtered-out items are excluded.
- **Open** acts on the selection. Multiple selected folders open in new tabs;
  files open in their default applications. Opening more than ten items asks
  for confirmation. SFTP and SMB files are handed to the desktop opener as URIs.
- **Empty Trash** requires confirmation. Cancel leaves the Trash untouched.
- **Open With** preserves quoted custom-command arguments, including program
  paths containing spaces. Custom launchers get distinct desktop-file IDs, so
  adding another command does not replace a previous one with the same name.

## Document templates

The File and folder-background **Create New Document** menus list regular files
from the configured XDG Templates directory. Selecting one copies its bytes to
a new document, preserves its extension, and starts inline naming. Existing
files, directories and symlinks are preserved; conflicting names get a numeric
suffix before the extension. The operation remembers the original pane, tab and
directory, so later navigation cannot redirect creation.

Discovery runs on a worker, refreshes about every five seconds, and can be
requested through **Refresh Templates**. Subfolders use relative labels such as
`Office/Letter.odt`; nested menu groups are not implemented. Discovery excludes
hidden files, symlinks and special files and is bounded to 128 templates, 10,000
examined entries and eight directory levels. A disabled/unconfigured Templates
directory leaves Empty Document available. Tests use explicit private template
roots and do not scan the real user Templates directory.

Stable URI-based template IDs and context-menu snapshots preserve the selected
template when the index changes while a popup is open. Removed IDs are refused.
Cached templates replaced by symlinks cannot copy their targets. Sources must
still be regular files when opened; templates are never executed.

Local and FM's live Gungnir adapter implement staged, exclusive publication.
Only a complete synced file is renamed into place, with no replacement of an
existing destination. Temporary staging is cleaned after failures. New documents retain template
read/write permission restrictions, subject to the process umask, keep owner
read/write bits before that mask, and drop executable/special bits. Template
copying has no separate progress or cancellation control yet. Remote creation from templates is unsupported; the
standalone VFS daemon protocol is unchanged.

## Remote deletion and regular-file copy corrections

Remote deletion now reports failed GIO exits and spawn errors, stops the batch
at its first failure, and checks cancellation before spawning. Cancelling an
active request kills and waits for its client process. This does not roll back
items already removed or guarantee that a remote daemon reverses completed work.
Hermetic executable client fixtures verify statuses, batch stopping, cancellation
and reaping; live remote-server behavior was not exercised.

Ordinary file copying opens a checked regular-file source with no-follow and
nonblocking flags, then verifies regular type, device/inode and available birth
time on its open descriptor. A replacement symlink or regular file is refused,
and a replacement FIFO cannot block the worker. These checks precede creation
of the copied destination data. Intended symlink copies still copy the link itself.

## Further defect corrections

Properties details, hashes, thumbnail generation, and application discovery run
on workers. Special files are not hashed, and both hash and checksum readers
reject FIFOs and devices without waiting for input. Cancelling Properties drops
late results; changing the selection does not change the dialog's original
file. The executable toggle is unavailable for directories, symlinks, and special
files. Launch failures are reported rather than silently closing the dialog or
claiming success after a failed default-opener exit.

Creation dialogs remember the original directory and preserve significant
filename spaces. Navigating while a dialog is open cannot redirect creation to
another folder.

Trash operations preserve restore records: rename, creation, and ordinary
transfer operations cannot detach entries from their `.trashinfo` files.
Restore accepts only top-level Trash URIs and absolute original paths. Existing
restore destinations are preserved. Orphaned Trash filenames get a fresh suffix
instead of blocking another item with the same name.

Desktop application discovery avoids symlink cycles, and `TryExec` requires an
actual executable file. Pattern matching was checked against an independent
reference implementation with libFuzzer and AFL++; fuzzing exposed and verified
the correction for names beginning with a literal `*`.

## Individual rename history and selection recovery

The Edit menu provides **Undo Rename** (`Ctrl+Z`) and **Redo Rename**
(`Ctrl+Shift+Z`) for individual local file, folder, and symlink renames. The
current window session keeps the most recent 100 supported renames. A new
successful rename clears redo; failed operations keep their history for retry.
Filesystem device/inode and birth-time metadata identify the original item;
history is unavailable where stable birth-time metadata is not provided. The
checks do not follow symlinks or keep deleted files open. Existing destination
names are preserved, and replacement items are rejected. Editing the same file's
contents does not prevent undoing its rename.

Rename editors remember their tab as well as their target URI. Switching tabs,
panes, or locations cancels editing. A changed row after sorting or refreshing
cannot silently redirect a stale editor. Failed renames preserve the original
selection. Silent directory refreshes remember the cursor's filename before
replacing the listing, preserving it when raw enumeration order changes.

Opening a path from the command line or a new window retains saved theme, view,
zoom, hidden-file, and sidebar preferences while using the requested locations.
Two path arguments show both panes, and file arguments retain their selection
when startup preferences are restored. Inaccessible explicit paths report their
error instead of being silently redirected to Home; saved inaccessible paths
still use recovery fallback.

History covers individual renames, with no copy/move/delete, batch rename, or
remote-operation undo. Concurrent external filesystem mutations can still race
the identity check and rename syscall; history is not a filesystem transaction.

## Session recovery and context menus

Session updates coalesce without dropping the newest snapshot when navigation
or settings change rapidly. Session writes use exclusive private temporary
files, sync contents and the parent directory, and clean up after failures.
Flush returns errors and can retry after the failure is corrected. Saving runs
on a blocking worker rather than the controller task.

The selected view mode and actual normal window size/maximized state now persist.
Closing the window waits for the final save. Legacy session files still load;
missing view mode defaults to detailed view. GUI checks verify a resized window,
compact view, both panes, and their tabs are restored after closing and reopening.

Context menus snapshot their inputs when opened. Selection/listing updates no
longer invalidate popup entry IDs and leave child menus empty. Mouse checks
exercise the Empty Document submenu and the existing Compress submenu, including
creating a ZIP and checking its contents. The modal custom-action callback also
uses a nonblocking command send so a full controller queue cannot freeze the UI.

Live Gungnir directory listing forwarding now watches cancellation while waiting
for a full output queue. A regression keeps the receiver alive but stops reading,
then confirms that cancellation terminates the listing.

## Test isolation correction

An older controller test set and reset the MP4 default application using the
user's real desktop configuration. Tests now receive dedicated temporary desktop
configuration and data directories, including all custom-launcher/default-app
writes. Test update checks and installations cannot perform real network or
package-update operations. This correction prevents future preference changes;
it does not recover a previous association that the older test may have removed.

## Validation

The current workspace has 190 passing tests, including 23 new regressions in the
template and copy/deletion pass after the previous 167-test baseline. Strict Clippy and formatting checks pass. Native GUI tests in an
isolated Xvfb display verify New Document, pattern selection, clipboard export and
import, copy/paste, tab/window shortcuts, resizing, and clean exit.

Earlier passes ran Miri and MemorySanitizer on the core helpers, AddressSanitizer
on backends and focused desktop/hash tests, ThreadSanitizer on concurrent local
operations, and Valgrind on Properties regressions. This pass additionally runs
AddressSanitizer and ThreadSanitizer on session regressions, and AddressSanitizer
on the blocked live-listing cancellation regression. Both fuzzers ran
bounded campaigns against the pattern matcher with an independent oracle.
The rename/startup continuation also runs 15 focused AddressSanitizer and
ThreadSanitizer checks against production helpers. Valgrind exercises 12 rename
controller tests with zero errors and no definite leaks; 48 bytes of possibly
lost runtime allocation remain reported. Xvfb checks exercise keyboard undo/redo,
collision/retry handling, text-editor shortcut isolation, mouse tab switching,
and explicit-path startup preferences. Detailed logs, the continuing audit's
complete tool matrix, and limitations are kept in the ignored `.bug-check/`
directory.

The template/deletion continuation runs all 65 backend tests under ASan,
including regular-file replacement and private-template permissions regressions.
TSan passes all 65 tests with the test harness serialized, and 52 non-archive
tests with its normal concurrent harness; individual threaded workloads remain
threaded. A concurrent archive run exposed a libc timezone-cache warning that
also reproduces in a standalone C `mktime` program. Disassembly confirms libc's
private lock is invisible to TSan. The warning and evidence are retained as a
system-library false positive; no production lock or suppression was added. Valgrind exercises
ten focused template/index controller tests with no errors or definite leaks;
48 bytes possibly lost remain reported. libFuzzer ran 2,860,965 inputs and AFL++
220,800 executions against template naming and URI round trips, using a separate
`std::path::Path` naming oracle. Native Xvfb mouse checks verify binary-template
contents, collision numbering, inline naming, both panes, File/background menus,
subfolder labels, and an index refresh while the popup remains open.

Undo/redo for operations other than individual local renames, nested template
menus, remote template creation and complete desktop extension compatibility
remain unfinished. Live remote servers and Wayland clipboard integration were not
available for end-to-end validation in this pass. Gungnir VFS's existing protocol
was sufficient; these changes are in Gungnir FM and its adapter.
