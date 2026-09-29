# Bokheim update discovery

This crate defines a provider-independent signed release feed and optional native
blocking HTTP adapter. The same verification and offer selection can be used by
Android, desktop, Kobo, or a browser worker supplying fetched bytes.

The default proposed endpoint is `https://updates.bokheim.se/v1/stable`. This change
does not provision or publish that endpoint. The desktop adapters remain parked;
this is discovery infrastructure, not the durable updater or settings integration.

## Trust and configuration

The workspace Cargo configuration supplies the production public key. Builds
outside that configuration must set `BOKHEIM_UPDATE_PUBLIC_KEY_HEX` to the trusted 32-byte
Ed25519 public key encoded as 64 hex characters. `BOKHEIM_UPDATE_KEY_ID` defaults to
`release-1`. The private signing key is never bundled; this repository configures public trust for its release identity. Verification fails
closed when the public key is absent. Never embed or commit the private key.

`BOKHEIM_UPDATE_ENDPOINT` overrides the endpoint at build time and, in the native
desktop adapter, at runtime. Other hosts can pass `DiscoveryConfig` explicitly.
Runtime endpoint changes do not change the trusted signing keys. Keys must be
provisioned through trusted application releases; for rotation, distribute clients
trusting old and new keys before switching signatures. The configuration API accepts
multiple keys; the current bundled configuration provides one key.

Endpoints and artifact URLs must use HTTPS without credentials or fragments.
The native client follows at most five redirects and rejects HTTP downgrades.
Download hostnames and filenames are not part of release selection.

## Protocol v1

Serve JSON in an envelope:

```json
{"key_id":"release-1","payload":"<exact JSON text>","signature":"<128 hexadecimal characters>"}
```

The Ed25519 signature covers the bytes `bokheim-update-manifest-v1`, one NUL byte,
and the exact UTF-8 bytes of the decoded `payload` string, in that order. Sign the
original string; do not reserialize it before verifying. Envelope size is capped
at 1 MiB. Unknown fields are rejected in v1 to prevent silently ignoring new
requirements; incompatible changes need a new protocol version.

Illustrative payload (replace dates, sizes, hashes, and URLs before publishing):

```json
{
  "protocol_version": 1,
  "channel": "stable",
  "sequence": 42,
  "issued_at_unix": 1790553600,
  "expires_at_unix": 1791158400,
  "application": {
    "version": "0.1.7",
    "name": "Bokheim 0.1.7",
    "schema_version": 75,
    "taxonomy_format_versions": [5],
    "artifacts": {
      "android-aarch64-apk": {
        "url": "https://downloads.bokheim.se/releases/0.1.7/android-arm64.apk",
        "bytes": 123456,
        "sha256": "<64 hexadecimal characters>"
      }
    }
  },
  "taxonomy": {
    "release_id": 12,
    "format_version": 5,
    "minimum_application_version": "0.1.7",
    "minimum_schema_version": 75,
    "artifact": {
      "url": "https://taxonomy.bokheim.se/releases/12/snapshot.sqlite3",
      "bytes": 234567,
      "sha256": "<64 hexadecimal characters>"
    }
  }
}
```

Both `application` and `taxonomy` may be null. Remove a withdrawn offer and publish
a higher sequence. Platform package IDs used by the desktop adapters are
`linux-x86_64-appimage` and `windows-x86_64-installer`. The contract supports other
IDs without coupling them to a hosting provider. Android APK signature continuity
and versionCode still require installer validation; this manifest is not a
replacement for Android package identity checks.

`offer` evaluates taxonomy against the selected application when an application
upgrade exists for the current target, or against installed capabilities otherwise.
The activation coordinator must additionally validate the actual migration path
and compatibility of retained taxonomy before approving a plan. These fields are
not permission to run downloaded migration code.

## Publishing

1. Build optimized release artifacts and validate them.
2. Upload immutable version-specific artifacts to any HTTPS host. Redirects from
   Bokheim-owned URLs may point to replaceable storage providers.
3. Populate byte lengths and SHA-256 digests from the actual uploaded artifacts.
4. Allocate an increasing sequence and bounded publication lifetime.
5. Sign and validate locally using a protected Ed25519 PKCS#8 private key:

   ```sh
   cargo run --release -p update-client --example sign_manifest -- manifest.json /secure/release-key.pk8 release-1 signed.json
   ```

6. Publish the signed envelope atomically only after its artifacts are available.
   Refresh expiration through a newly signed, higher-sequence publication. Keep
   pinned artifact URLs available for previously approved downloads.

GitHub may host files or run builds; clients neither call its release API nor
interpret its release names or asset filenames. Existing CI artifact publication
is unchanged; publishing the signed feed is a separate deployment step.

## Host integration still required

Persist the highest verified sequence (scoped to channel/trust configuration),
authenticated source envelope, and approved plan. Pass that sequence to `verify`
or `blocking::discover`; reject older publications. The parked desktop adapter
currently passes zero and must gain this durable state before activation wiring.
Equal-sequence content changes should also be rejected by comparing the persisted
envelope digest. Check cached metadata expiration before using it to offer a new
update. An already-approved plan has separate durable authorization and must not
be silently replaced by later discovery results.

Add background polling/conditional requests, staging, restart/install activation,
withdrawal handling for prepared plans, and automatic recovery in the coordinator.
Discovery failure is not a user-action prompt. Native HTTP code authenticates
metadata and bounds downloads; callers own staging-file cleanup and disk durability.
Application installation stays platform-specific. Library migrations stay compiled
into the application. No server endpoint, signing secret, scheduler, or live update
UI is installed by this crate.

## Validation

```sh
cargo test --release -p update-client --features native-state
cargo check --release -p update-client --target wasm32-unknown-unknown
```

## Hosting: static files are sufficient

No database or new application service is required for discovery. An existing
HTTPS web server or static object host can serve the signed JSON envelope at
`/v1/stable`. Serve manifest responses with `Content-Type: application/json` and
cache revalidation (ETag or Last-Modified, with a short cache lifetime). Publish
versioned artifacts with immutable URLs and long-lived caching. Enable public,
credential-free CORS for manifest and artifact requests if browser clients fetch
them across origins.

The publisher owns release selection, signing, and atomic publication. The host
only serves bytes; it never needs the private signing key. Use the existing Bokheim
HTTPS infrastructure if convenient. A separate `updates.bokheim.se` hostname makes
it possible to move the static host without changing clients. Provisioning DNS/TLS
and deploying signed files is still required before this proposed endpoint works.

## Durable coordinator (implemented)

`coordinator::Coordinator` separates discovery, explicit approval, preparation,
readiness, and startup activation. It persists before publishing a transition.
`Store` uses compare-and-swap revisions so stale processes cannot overwrite newer
approvals. Signed envelopes, the highest publication sequence, the approved plan,
completed artifacts, retry deadlines, and diagnostic errors survive a restart.

The `native-state` feature adds `native_state::SqliteStore` under the private
`updates/` directory, a process lock, discovery ticks, and verified file staging.
Completed downloads are reused after interruption; incomplete downloads restart.
The host must provide actual extra installation/backup/workspace estimates and a
safety reserve for the staging volume. Other volumes need their own preflight.
File hashes are rechecked before activation, not merely trusted from a database
flag. A corrupt staged file returns to preparation and waits for another launch.

A host integration follows this order:

1. Open the coordinator with a unique ID for the current process launch and the
   actual installed application, schema, and taxonomy versions.
2. Under `UpdateLock`, if `activation_due()`, validate `staged_paths()` and call
   `begin_activation()` **before opening libraries**. Resume interrupted activation
   using the platform's installation and transactional migration checkpoints.
   Android APK plans cannot begin data activation until the new APK version runs.
3. The host applies and validates its actual installation/migrations, then calls
   `activated(actual_versions)`. Do not mark completion merely because files were
   downloaded. On failure recover the prior usable state; only then quarantine a
   defective release. A corrected artifact plan becomes an offer needing approval.
4. In a background worker call `check_for_updates` when due. It uses the six-hour
   discovery interval and retries failures with backoff. Network work never runs
   inside a library transaction or on the UI thread.
5. On explicit Update call `approve`, then run preparation ticks as due. A Ready
   state never activates in the same launch, including when preparation resumed
   after a previous crash. The Restart action belongs to the platform host; Android
   uses Install when the plan contains an APK.
6. Forward `view()` changes to the settings UI. Native errors that escape staging
   must be diagnosed and scheduled by the host; do not show generic error dialogs.

`browser-ui::UpdateControls` and `AppServices::with_updates` now bind the shared
view/action model to one conditional row above Account in Settings. The state is
shared across windows; changes invalidate the settings page. Space deficits are
rounded up by at most one MB. Technical errors never enter the view model.

**Linux host wiring is implemented** in `client/update-host` and
`apps/desktop-gpui/src/linux_updates.rs`. Signing-key-configured Linux builds bind
Settings, poll/stage in the background, prepare database copies with the candidate
executable, and use journaled next-launch activation with automatic rollback.
See `client/update-host/README.md` for verification and the remaining production
rollout checks. The old desktop updater modules remain unused.

Android Settings/startup wiring is implemented with a private migration service
process and explicit OS installer/permission actions. Device validation remains
pending; see `apps/desktop-gpui/android/README.md`.

The shared activation journal and staging now support Windows file operations.
`native_files` flushes writable handles and uses write-through file replacement;
Windows CI exercises recovery and a destination held open by another process.
Windows offers no unprivileged POSIX directory-fsync equivalent. This layer does
not yet implement Windows application supervision or the installer/Settings adapter.
The API choices follow Microsoft's
[MoveFileExW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw)
and [FlushFileBuffers](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers)
contracts.

Windows application activation, browser storage/activation, production signing
configuration and endpoint publication are still pending.

`supervisor::wait_for_trial` watches the journal's matching healthy-startup commit
while observing the spawned process. It returns a stopped outcome only after the
child has exited/reaped, including timeout and receipt-read failures. Callers must
then consult the journal again: a commit racing a timeout must never be rolled
back. Failure to prove the process stopped is an error, not permission to restore
its files. Linux uses this shared supervisor; Windows application wiring remains
pending. The supervisor is tested with real child processes on Linux and Wine.
