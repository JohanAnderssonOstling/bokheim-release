# Book identity

A PDF identity is the BLAKE3 hash of its original bytes, before Bokheim writes
`BokheimContentHash` into its document root. Metadata edits retain the marker and
therefore the identity. An external tool that deletes the marker removes this
persistence guarantee.

An M4B identity is the BLAKE3 fingerprint of its audio: the encoded samples plus
the timing and codec configuration needed to decode them, excluding chapter
tracks and the movable chunk offsets that a tag write shuffles. Nothing is
embedded, so there is no marker to lose and any later revision recomputes the
same identity. Identical audio published with different tags is one book;
a re-encode is a different one.

`read` derives the identity a file already carries, `identify` falls back to
hashing a file that carries none, and native `ensure` atomically embeds a missing
PDF marker. Failed parsing or writing leaves the original unchanged. Files whose
identity cannot be derived use the BLAKE3 hash of their unchanged bytes --
read-only and unparseable PDFs, and audio that cannot be fingerprinted. That
audio is also audio `enrich_m4b` refuses to rewrite, so its bytes stay stable.
All identity readers use this same rule. If an external tool later changes such a
file, its byte identity changes.
PDF updates append a document-root revision without rewriting original streams.
Bokheim never writes to an M4B at all: the identity is derived, and enrichment
belongs in the library database, where a wrong result can be reverted without
touching a file the user owns.

Encrypted PDFs that open with an empty password are supported. The appended
root uses the original encryption key; the encryption dictionary, file IDs,
permissions and original bytes are preserved. PDFs requiring a non-empty
password use byte identity without modification. Reader annotations are stored
in the library database and do not rewrite the PDF.

The identity is not an integrity checksum of later file contents. Sync uploads
carry `x-bokheim-content-checksum`; the server verifies bytes under that immutable
CAS address and maps the book identity to it within the authenticated account.
Downloads expose that checksum and use it as their ETag. Clients verify both the
byte checksum and the derived identity before publishing a download. Because an
M4B identity is derived rather than declared, the server proves it by
recomputing the fingerprint from the bytes it stored. A native scan
of a changed file queues its current revision for upload; local copies on other
devices are not overwritten automatically.

The revision table is part of the baseline schema, and migration
`17-user-book-revision` adds it to a database created before revisions existed.
No older transfer protocol is supported: uploads and downloads require an
explicit `x-bokheim-content-checksum`. The client and server must be released
together.
