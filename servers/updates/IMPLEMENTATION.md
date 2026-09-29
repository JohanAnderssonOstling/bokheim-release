# Update feature completion tracking

The objective is the complete cross-platform update feature, including delivery
and operational verification. A passing Linux fixture is not completion.

## Current shipping audit (2026-09-29)

**Current checkout supersedes earlier migration evidence:** the desktop package
is now `0.1.0`, `LIBRARY_SCHEMA_VERSION` is `1`, and database initialization rejects
older nonzero schemas. The intended fresh-start versus existing-library upgrade
policy has been requested before preparing a release commit. Earlier schema-80/82
migration results do not validate this changed candidate.

The development checkout contains hundreds of mixed uncommitted changes. The
local GPUI-Fork HEAD differs from the release pin and has 4 uncommitted entries;
HtmlEngine and HtmlViewCore match their pins but have 58 and 30 uncommitted entries
respectively at inspection. The existing clean-source release gate correctly
refuses to attest this state. A release needs a coherent committed source set and
matching sibling pins before workflows are pushed and dispatched; publishing a
pipeline-only subset would not establish a buildable updater release.

This summary takes precedence over the chronological verification notes below.
The feature is not ready for production publication yet.

### Remaining implementation

- Android APK delivery is now wired into the existing release workflow, draft
  artifact upload and signed-manifest preparation. A separate instrumentation APK
  reads the compiled runtime metadata on a dedicated ARM64 test device. APK signing
  is pinned to the expected production certificate and remains separate from feed
  signing. The workflow requires a configured `bokheim-android` device runner.
- These new Android pipeline paths have not run in GitHub or on a device. Local
  All 20 pipeline Python tests and release instrumentation Java compilation pass.
  The receipt test rejects failed instrumentation and a mismatched APK signer;
  workflow dependencies and required Android Rust build settings are checked.
  Workflow YAML and Python syntax checks pass. The Rust Android release check
  now passes after refreshing the stale PDFium preparation receipt and waiting
  for concurrent workspace builds. These checks do not prove an APK build or a
  device update trial.

### Release configuration and operations

Feed identity `release-1` is now generated locally at
`~/.local/share/bokheim-signing/update-feed-release-1.pk8` (mode 0600).
Its public configuration is `~/.config/bokheim-updates/publisher.json`.
The workspace Cargo configuration and desktop release workflow now inject this
public key into future application builds. The optimized signer accepts standard
OpenSSL Ed25519 PKCS#8 v1 as well as Ring v2; actual signing and publication into
an isolated directory verified the new identity successfully.

Public setup files are staged on `johan@192.168.1.68` at
`/home/johan/bokheim-update-setup.zHRjnTox`. No private key was uploaded. The
candidate Caddy file adds update hosting and taxonomy artifact routes to the
currently installed configuration, preserving its other routes. Syntax adaptation
passed; installation will validate against the server's actual environment and
refuse a changed base Caddy file. The staged initial sequence-1 feed advertises no
updates and expires 30 days after preparation. It is not yet publicly installed.

Activation awaits DNS for `updates.bokheim.se` and interactive server sudo.
The intended DNS alias is `updates.bokheim.se CNAME app.bokheim.se`; the latter
currently resolves to `158.174.48.8`. Noninteractive sudo does not permit update
host provisioning or Caddy installation. Run the staged installer only after DNS
is configured; then verify the public HTTPS feed before advertising any release.

Published Android signing audit: downloaded the public v0.1.6 `bokheim-arm64.apk`
and verified its signature with Android SDK apksigner. Its certificate SHA-256 is
`97220997a830cbd326ab178a7467b1c85e7af172f5560b3ec29e54e457b384f0`,
with subject `CN=Johan Andersson Östling, OU=Unknown, O=Unknown, L=Uppsala, ST=Uppland, C=SE`.
It differs from both the local debug certificate and the key in
`~/.local/share/bokheim-signing/bokheim-release.jks`. The local Gradle settings
added for that nonmatching key have been removed; it must not sign updates for
existing installations. After the user updated `~/android.txt`, `~/bokheim-release.jks` (alias
`bokheim`) was successfully opened: its certificate exactly matches the published
v0.1.6 APK. A temporary certificate request also verified private-key access.
Local `~/.gradle/gradle.properties` now references that keystore and password file;
no password was copied into Gradle properties or the repository. This verifies
signing identity continuity, not installation/migration on an Android device.
The Gradle support for private password files remains implemented and validated.
No secrets were uploaded. GitHub CLI is absent and no usable GitHub credential
was found in the standard environment, CLI configuration or Git credential helper.

- Configure the production feed trust/signing identity, APK signing identity,
  publishing account and public config. The last read-only host inspection found
  neither `/etc/bokheim-updates.json` nor `/srv/bokheim-updates`.
- Run the existing signing/publication path against staging, then distribute an
  updater-enabled bootstrap and verify a subsequent update before broad rollout.
  Local orchestration tests do not establish live publication or deployment.

Local publication trial: the existing optimized `sign_manifest` and
`update-publisher` executables successfully signed and published a fixture feed
and artifact into an isolated directory, accepted an identical retry, and rejected
a lower sequence while preserving the published feed. Evidence is in
`/tmp/bokheim-publication-trial-oe7via1d/result.txt`. The disposable private key was
removed. This exercised the real local executables; it did not exercise GitHub
orchestration, HTTP serving, production credentials or application installation.

### Verification still needed

- Packaged native update/recovery trials, including Android system installer and
  process lifecycle behavior, remain unverified. No Android device was attached
  at the last inspection. Existing fixture tests are narrower evidence.
- Browser startup, populated-library migration and interrupted taxonomy recovery
  have local optimized-release coverage. An archived historical release upgrade
  and a real hosted rollout have not been exercised.

Do not add another verification framework to close these gaps. Use the existing
build, publication and update paths. Android package delivery is wired; production identities and the device runner
remain external configuration dependencies.

## Implemented and locally verified

- Signed provider-independent discovery, explicit approval, durable staging and
  retry state. Separate internal taxonomy revision, application/schema version
  coupling, one conditional Settings row.
- Linux activation on a later launch, isolated database migration and taxonomy
  reprojection, backups, trial startup, background-work barrier and rollback.
- Taxonomy reprojection handles removed and newly matching subjects and preserves
  manual concept assignments without generating sync edits to canonical metadata.
- Static publisher verifies signatures, replay floor, immutable local artifacts,
  external public artifact bytes and atomic feed promotion.
- GitHub preparation checks a successful desktop release workflow, exact-commit
  shared-suite status, original workflow packages against release assets, actual
  package compatibility and bundled signing trust. Writes reviewable unsigned
  manifest and provenance. Supports Linux AppImage and Windows update ZIP.
- Desktop CI injects configured public trust and retains actual AppImage metadata.
- Shared staging/activation file operations now support Windows: writable flush
  handles, write-through replacement and journal retirement, plus locked-file
  retry coverage. Windows CI runs these tests. Application supervision/Settings
  activation on Windows is implemented; native GUI rollout trials remain.
- Windows release builds also produce a complete runtime ZIP for self-update,
  separately from the bootstrap installer. `windows_package` validates Windows
  path semantics and expanded size before extracting into a fresh directory.
  The publication-preparation tool includes Windows after checking ZIP metadata.
- Shared startup trial supervision now observes the healthy commit, stops/reaps
  hung or unreadable trials before permitting recovery, and handles normal early
  exits. Linux uses this implementation; Windows can use the same process contract.
- Android installer boundary verifies staged path/bytes/APK identity/version,
  exposes a restricted FileProvider and explicit installer/permission intents.
  Update state is excluded from backups/device transfer. Release APK versions
  derive from Cargo; production packaging requires configured signing, with a
  separate explicit local-signing option for optimized test APKs.
- Android data host in `client/update-host/src/android.rs` now gates data
  activation on a later launch and the installed APK version, reverifies staged
  packages, prepares isolated database copies through a host-supplied helper,
  journals promotion, restores interrupted trials and commits on healthy startup.
  It is wired into the Android entry point and shared Settings row. The migration
  callback uses a non-exported service in a fresh private process and awaits Binder
  death; in-process taxonomy switching would violate the snapshot invariant.
  Explicit installer/permission actions and a separate-process Restart activity
  are connected. Real-device verification remains outstanding.

Most recent verification (2026-09-29): updater 19 Rust release tests, publisher
10 Rust release tests, 10 CI/preparation Python tests; desktop optimized release
check and workflow YAML parsing passed. Earlier Linux host, database and runtime
tests are documented in their module/test sources. Live CI has not been run for
this worktree.
Android release unit suite: 41 passed, including four installer tests and three
helper/service-boundary tests. Missing production signing was verified to fail
`requireReleaseSigning` as intended. The Android Rust release
cross-check passed after fixing a JNI string conversion and restoring the missing
workspace `blake3 = "1"` declaration required by `book-metadata`. No Android device
was attached at last check. AppImage packaging no longer embeds the legacy GitHub
zsync update URL; the signed feed owns in-app discovery.
The latest native host suite has 7 passing release tests, including Android APK
consent/version gating, interrupted data recovery, corrupt staging retry and
quarantine of an APK whose identity/version is invalid. The Java release suite
still has 41 passing tests after that rejection path was connected.
Native file portability verification: 20 Linux updater release tests passed, and
21 Windows GNU release tests passed under an isolated Wine prefix (including a
locked destination and repeatable WAL-database recovery). This is not a native
Windows application update trial; the Windows CI job has not been run here.
After adding complete Windows ZIP packaging/extraction, verification passed with
22 Linux updater tests, 23 Windows updater tests under Wine and 11 Python pipeline
tests. The desktop workflow YAML also parses successfully.
With shared process supervision added, the updater suite passes 25 Linux release
tests and 26 Windows release tests under Wine, including real child processes for
healthy acknowledgement, early exit, timeout and unreadable journal cases.
The Linux desktop release check passes with the shared supervisor wired in.
Windows CI filters to filesystem, journal, process and package tests; portable
signature/coordinator tests remain in the own-runner shared suite.

## Required before completion

1. Real packaged Linux update trial with two updater-enabled release AppImages:
   Settings approval, current run undisturbed, restart, actual library migration,
   taxonomy/assignment changes, successful startup acknowledgement, interrupted
   preparation/activation, failed startup recovery, storage/permission blockers.
   Existing host integration uses a fixture executable, not a GUI AppImage trial.
2. Native Windows GUI update/recovery trial with two release versions. The
   activation host, helper handoff and Settings binding are implemented; real
   executable fixture trials pass under Wine.
3. Android real-device verification of installer/permission handling, helper and
   restart process lifecycle, later-launch migration and rollback. Provision the
   actual production signing identity and test two release APKs. Code downgrade
   cannot be performed silently by this app; data rollback is implemented, while
   a faulty APK requires a corrected newer signed APK. No app store dependency.
4. Verify automatic browser updates across two website deployments with real data.
   Latest bundle loads on navigation; migrations and bundled taxonomy reprojection
   finish before the app opens. Older storage owners block migration with a clear
   close-other-tabs message. Startup retries once, then offers Reload. Browser
   approval, custom caching, snapshots and rollback journals were removed per the
   revised user requirement. Check assignment preservation and interrupted migration.
5. Add completed platform packages to release preparation and verify their actual
   runtime compatibility/trust metadata. Extend artifact build/release pipelines
   where missing. Platform tests on GitHub, shared suite on the trusted own runner.
6. Production public/private key provisioning, protected signer credentials,
   shared runner/gates, updates DNS/HTTPS and installed publisher configuration.
   Current example config has a placeholder key. No live deployment performed.
7. Run reviewed release signing/publication automation against a real staging
   endpoint. The orchestration command is implemented and has local gate tests;
   production credentials/public artifacts and an actual staging run remain.
   Public GitHub assets must exist before promotion; do not advertise draft assets.
8. Publish the initial updater-enabled bootstrap via existing distribution, then
   verify a subsequent production update and feed renewal/availability monitoring.

Keep the private signing key outside this repository and serving host. Live
publication changes what users are offered and needs a concrete reviewed release.


### Windows static PDFium (2026-09-29)

Windows x64 MSVC now links the pinned PDFium 8046b /MT library directly into
the executable. Archive, library and supplemental license notices are verified.
The installer and update ZIP no longer ship PDFium runtime files; ZIP validation
requires the executable without requiring a DLL. The Windows transaction tests
now exercise executable-only replacement and registry rollback. No PDFium DLL
copy, loading override, independent backup, or version matching is needed.
The external helper remains necessary for replacing the running executable.

Validation: 26 optimized PDF core tests and the relocated standalone executable
render/search test passed under Wine. The latter's PE imports contain Windows
system DLLs only, with no PDFium or VC runtime DLL. Python preparation tests
(including tampered notices) and deterministic ZIP tests pass. Native Windows
GUI testing and the remaining update helper/UI integration are still pending.


### Windows migration process and recovery verification

The release executable now shares the headless update metadata and candidate
migration entry points between Linux and Windows. WindowsHost can invoke the
candidate as a separate process and requires its exit before inspecting prepared
databases. Migration success, nonzero exit and timeout use the shared supervisor;
timeouts terminate and reap the child. Linux uses the same migration supervisor.

The Windows MSVC release host tests now run under Wine: interrupted executable
and registry recovery, healthy commit/cleanup, and a Windows-only locked obsolete
file test all pass (3 tests). A temporary cleanup failure no longer prevents app
startup; a later launch retries it. These are host-level tests, not evidence of
complete Windows desktop ownership handoff or Settings integration.


The migration supervisor's four release tests passed on Linux and Windows MSVC
under Wine. The Linux desktop release check passed after sharing the headless
entry points. Unused immediate-install desktop adapters and their direct HTTP
dependency have been removed.

Full Windows desktop cross-check reached GPUI's Linux-hosted build limitation:
`gpui_windows` does not generate `shaders_bytes.rs` outside a Windows build host.
The resource compiler prerequisite was resolved with
`RC_x86_64_pc_windows_msvc=x86_64-w64-mingw32-windres`. No shader output has been
fabricated or substituted; full Windows desktop validation remains outstanding.


### Windows desktop handoff and pipeline (2026-09-29)

Windows startup and Settings now use WindowsHost. The headless helper holds a
launch gate, opens a SYNCHRONIZE handle before acknowledging the original
process, waits for its exit, and takes exclusive desktop ownership before
promotion. Every child gets a fresh launch directory and PID-bound permit.
The helper keeps the gate until the new trial commits, or until a restored
application acquires ownership. Failed process creation and crashed trials
restore backups; uncertain process termination never permits recovery.
Helpers and obsolete temporary files are cleaned only under startup ownership.

The optimized Windows fixture runs real EXEs through both successful handoff
and a crashing trial, verifying executable and registry rollback. Both scenarios
pass under Wine. CI runs this fixture and the Windows host/process tests.
The complete desktop Windows release check also passes after compiling all 18
GPUI shader entry points with D3DCompile optimization level 3 under Wine.
The reproducible cross-shader tool records source/output hashes.

Windows release artifacts now retain compatibility/trust JSON read from the
actual extracted ZIP executable. The preparation tool validates Windows and
Linux packages against the same version/schema/taxonomy compatibility and trust
before producing an unsigned manifest. All 12 Python pipeline tests pass.
No release was published, signed or deployed. Native GUI/device trials and
production provisioning from the remaining checklist still apply.


### Browser design simplified

The earlier custom browser updater was superseded by automatic deployment updates.
Its approval, caching, snapshots and trial journal code/tests were removed. Current
browser behavior and remaining verification are documented in
`apps/web-gpui/updates/README.md`. Native update requirements above still apply.

### Browser release verification (2026-09-29)

The optimized full application passes the Chromium deployment regression:
one worker initializes after a 65-second delay without being restarted, an old
tab keeps its bundle and storage ownership, a newer deployment reports that
ownership conflict, and closing the old tab allows the new deployment to open
on reload. Injected worker startup failures verify one automatic reload, a
persistent failure showing Reload, and successful recovery after an explicit
reload. The old release reproduces the premature automatic reload in the slow
initialization test. Removed both the coordinator's 15-second and client's
60-second readiness deadlines; worker/transport errors still propagate.

The Firefox startup suite also passes when access to sessionStorage itself
throws: automatic retry is disabled and the manual Reload action remains.
Optimized web-release packaging and the release worker example check with
web-runtime-tests passed. The CI check now targets the worker examples in
web-backend-worker rather than their former app package location.

The deployment test uses one compiled application under two bundle identities
with initially empty browser storage. It does not prove historical schema
migration, assignment preservation over a taxonomy change, or interrupted
migration with populated libraries. Those checks, native GUI/device trials and
production publication/provisioning remain required above. Nothing was deployed.

### Populated browser library recovery (2026-09-29)

`migration.browser-test.mjs` now passes against the optimized release app and
real OPFS. Its optimized fixture creates a library through the registry, ingests
three books through normal sync ingestion, and queues a reading-position edit
through the database API. It reconstructs the v80 outbox shape and marks an older
taxonomy projection with unmatched, stale and manually assigned subjects.

Two independent browser contexts verify direct startup migration from v80 and
recovery after terminating the worker inside a taxonomy transaction. A temporary
SQLite trigger signals after a real assignment deletion, then parks the worker
until termination. Reopening verifies transactional rollback before the release
app completes reprojection. Both cases check the current schema, assignment
changes, preserved manual concept, unchanged canonical metadata/sync registers
and pending edit, and idempotence across two release-app launches.

The fixture reconstructs the historical schema shape; it is not an archived old
application or a second compiled taxonomy release. Together with the separate
deployment test this verifies the browser startup/update mechanisms locally.
Historical packaged-release rollout remains distinct from these constructed
fixtures. Native GUI/device trials and production provisioning/publication remain
open as listed above.

The web platform CI job now builds the optimized app and fixture and runs both
release browser regressions. Its obsolete storage-contract-tests feature flag
was removed. The corrected standalone web platform release check, optimized
fixture build, browser regression, script syntax and workflow YAML checks passed
locally. Live GitHub CI has not run for this worktree. Nothing was deployed.

### Reviewed publication and host provisioning (2026-09-29)

`publish-github-release.py` binds the reviewed manifest and provenance with a
combined digest, retains the exact bytes for signing, rechecks the source workflow
and latest exact-commit shared gate, requires a public stable release with the
matching source receipt/packages, signs locally, and rechecks the shared gate
before invoking the publisher. Remote publication verifies the public feed bytes;
local staging uses the same publisher against an isolated directory. Failed or
uncertain publication retains the signed bundle for inspection/idempotent retry.
Private keys and unrelated files are never copied into that bundle.

`update-publisher --check-config` validates public trust/routes without publication.
`provision-publisher.sh` prepares the publication directory and root-owned public
config for an existing non-root account, refusing conflicting ownership/trust.
It changes no credentials, sudo rules, Caddy configuration or feed.

Verification: all 18 Python pipeline tests pass, including six new orchestration
tests for reviewed-byte changes, changing preparation inputs after review, draft
or wrong-source releases, superseded shared results, uncertain publication,
public-feed mismatch and local staging selection. Those tests mock external
commands/network; they do not prove a live signed publication. The optimized
publisher build passed; its actual config-validation CLI accepts a valid public
config and rejects the placeholder key. Shell syntax and diff checks pass.

A read-only SSH check of the configured deployment host 192.168.1.68 confirmed
that /etc/bokheim-updates.json and /srv/bokheim-updates are absent. No Android
device is connected. Production account/key/config selection has been requested;
no host provisioning, production signing, publication or deployment was performed.

Concurrent sync changes introduced explicit book-creation declarations. The
browser migration fixture now provides those declarations through normal sync
ingestion; its optimized rebuild and the populated-library release regression
both pass with that adjustment.

### Live feed activation (2026-09-29)

The operator ran the staged installer successfully. Public trust and the publication
directory are installed, Caddy validated against its actual environment and
reloaded, and the initial sequence-1 feed is published. Fetching the public HTTPS
URL from the server verified its Ed25519 signature, validity interval, protocol,
channel and exact staged bytes. It advertises no application or taxonomy update.
Local normal DNS resolution has not caught up; the direct-IP HTTPS check retains
hostname certificate verification. This establishes feed hosting and signing, not
a packaged application rollout. CI credentials, Android device verification and
the updater-enabled bootstrap/subsequent update remain outstanding.

### GitHub release destination correction

Per user direction, desktop/Android and Kobo draft releases now target the source
repository, `JohanAnderssonOstling/bokheim`. Only publication jobs receive
`contents: write`; they use the built-in Actions token and target the tested source
SHA. The separate release-repository token is removed. Local publisher config and
the documented example now allow the bokheim release-download path; the changed
public config is staged as `publisher-bokheim.json` in the existing server setup
directory and is not yet installed. All 20 pipeline tests and workflow structure
checks pass. GitHub CLI was installed from its official release with checksum
verification; device authorization is pending. Anonymous access to the bokheim
repository API returned 404, so visibility and anonymous artifact availability
need checking after login before choosing final artifact delivery URLs.

### Authenticated GitHub setup and private release delivery

GitHub login succeeded as `JohanAnderssonOstling` with repository admin access.
The repository is private. Existing `ANDROID_KEYSTORE_BASE64`,
`ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS` and `ANDROID_KEY_PASSWORD`
secrets are reused by the workflow; their values cannot be read back, so the APK
certificate gate remains authoritative. No private keys were uploaded this turn.
Created the `android-release` environment and configured the feed public key,
key ID, HTTPS endpoint and verified published APK certificate as repository
variables. There are no registered Actions runners; runner-machine selection has
been requested. Workflow edits are local and have not been pushed or executed.

Preparation now copies authenticated, verified GitHub release packages into the
publisher bundle and advertises immutable `updates.bokheim.se` URLs. Signing
rechecks the package hashes and sizes before copying them into the signed bundle.
The server and users need no GitHub credentials; private source releases are
supported. All 21 Python pipeline tests pass, including prepared-package tampering.
No new server allowlist is required: the existing local artifact origin already
supports these URLs. The unneeded staged allowlist change was removed and local
public configuration restored to match the installed server configuration.

### Local workstation runner (2026-09-29)

With user authorization, removed obsolete native debug build output, the completed
update Wine prefix and the superseded browser release fixture. This freed about
9 GB on the home disk and 772 MB in tmpfs. Source, library data, current optimized
builds and unrelated test outputs were retained.

Installed official Actions runner v2.337.0 with its published SHA-256 verified,
registered `johan-82sn-bokheim`, and enabled its user service. GitHub reports it
online and idle with `bokheim-shared` and `bokheim-local` labels. It has its own
work directory. Rust 1.95.0 is installed; the default developer toolchain is
unchanged. The service runs during the user session (lingering is not enabled).

Shared tests use the existing isolated native PostgreSQL lifecycle, without
Docker. Trusted Linux/web/Android checks and APK builds are routed locally;
pull-request checks retain disposable hosted runners. Windows and AppImage
packaging retain their required hosted OS environments. No Android device runner
label is assigned until a device is available. Workflow YAML and 21 pipeline
tests pass; local workflow edits are not yet pushed and no release suite or
publication job has been dispatched.
