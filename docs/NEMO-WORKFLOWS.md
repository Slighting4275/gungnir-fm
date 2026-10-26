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

The current workspace has 152 passing tests, including 10 new regressions in the
creation and session-recovery pass after the previous 142-test baseline. Strict Clippy and formatting checks pass. Native GUI tests in an
isolated Xvfb display verify New Document, pattern selection, clipboard export and
import, copy/paste, tab/window shortcuts, resizing, and clean exit.

Earlier passes ran Miri and MemorySanitizer on the core helpers, AddressSanitizer
on backends and focused desktop/hash tests, ThreadSanitizer on concurrent local
operations, and Valgrind on Properties regressions. This pass additionally runs
AddressSanitizer and ThreadSanitizer on session regressions, and AddressSanitizer
on the blocked live-listing cancellation regression. Both fuzzers ran
bounded campaigns against the pattern matcher with an independent oracle.
Detailed logs, the continuing audit's complete tool matrix, and limitations are
kept in the ignored `.bug-check/` directory.

Operation-history undo/redo and complete desktop extension compatibility remain
unfinished. Live remote servers and Wayland clipboard integration were not
available for end-to-end validation in this pass. Gungnir VFS's existing protocol
was sufficient; these changes are in Gungnir FM and its adapter.
