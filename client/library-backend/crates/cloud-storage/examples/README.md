# Live Google Drive checks

`google_drive_live` is an opt-in Linux integration runner using the production
OAuth, connection, upload, scanner and range-reader implementations. It opens
the system browser and checks the returned account against the supplied email
before accessing Drive.

Prepare a private state directory containing these generated fixtures:

- `fixtures/book.epub`: a valid EPUB larger than 256 KiB, with a nonempty ZIP
  comment. The size exercises resumable uploads; the comment permits a valid
  same-size content replacement without changing the book itself.
- `fixtures/audio.m4b`: a small valid audiobook clip.
- `fixtures/cover.png`: a small valid PNG.

Run from the workspace root:

```sh
cargo run --release -p cloud-storage --features google-drive --example google_drive_live -- \
  /private/path/desktop-client.json /private/path/test-state tester@example.com
```

The account must be allowed as an OAuth test user while the project is in
Testing. Select the Drive permission on Google's consent screen. Cached
credentials stay in the private state directory and allow subsequent runs
without another interactive sign-in. Keep both the client JSON and state
directory outside Git.

Each run creates its own folder in My Drive. Only fixture files in that folder
are renamed or replaced, and the folder is moved to Trash after the checks,
including on a behavior-check failure. `report.json` records the results and
whether cleanup succeeded. This runner exercises the storage backend; it does
not exercise the application UI or decode/play the audiobook.
