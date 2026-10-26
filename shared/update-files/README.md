# Update file operations

The native updater, Kobo on-device updater, and standalone USB installer share
this crate's file operations. It contains bounded reads, streaming copies with
inspection/progress hooks, atomic staging, permission preservation, destination
checks, flushing, replacement, removal, write probes, and volume comparison.

`with_atomic_file` writes and flushes a complete temporary file before replacing
the destination. It rejects nonregular destinations both before staging and
immediately before replacement, and removes temporary output on failure.
`copy_atomic` preserves source permissions; `write_atomic` accepts a reader and
reports copied bytes. Callers supply temporary names because the Kobo firmware
recovery script recognizes its suffix and the USB installer uses unique names.

Replacement uses rename and directory fsync on Unix, and write-through
`MoveFileExW` replacement on Windows. The Windows path never falls back to a
cross-volume copy or deferred reboot replacement. `update-client::native_files`
retains its existing string-error API as adapters over these operations.

Artifact hashing and authenticated size checks remain in `update-protocol`,
which uses the copy inspection hook before writing each chunk. Its
`stage_verified` function combines verification with atomic staging for all
native downloads. Format-specific ZIP and Kobo package checks remain with their
respective parsers.
