# Update hosting and publication

The public update service is static HTTPS, served by the existing Caddy server.
It requires no update API process or database. `update-publisher` is a deployment
tool, invoked by an operator or CI. Clients depend on the signed Bokheim protocol;
GitHub is optional build/artifact storage.

## Release flow

1. Build optimized application packages for each supported target. Run release
   tests and platform package signing. Schema migrations ship in the application.
2. If publishing taxonomy, prepare and validate the snapshot with its final
   release ID. Include the compatible application/schema/format requirements.
3. Prepare a manifest using the types in `shared/update-client/src/lib.rs`.
   Set each artifact's exact bytes and SHA-256, immutable HTTPS URL, and a new,
   monotonically increasing publication sequence. Use a bounded expiry.
4. Review and sign that exact manifest using the existing
   `shared/update-client/examples/sign_manifest.rs` release utility. Keep its
   Ed25519 private key in the release signer, never on the hosting server or in
   artifacts. OS package signing is additional to this manifest signature.
5. Upload the signed bundle and run the publisher. It verifies trust, validity,
   sequence, length and digest; copies and flushes immutable artifacts; archives
   the envelope; then atomically replaces `/v1/stable` **last**.
6. Verify the public feed and artifact URLs. Clients discover the new offer on
   their next successful check. They still need explicit Update consent; activation
   waits for a later launch (Android APK installation uses its system installer).

Application and taxonomy releases can be published separately. A manifest is the
complete current offer: retain the unchanged component when publishing the other.
Do not advertise a new app until all promised platform packages are available.
There is currently one application version per manifest, so this is a coordinated
platform release, not independent application versions per platform.

### Bundle layout

```text
bundle/
  manifest.signed.json
  artifacts/
    updates/releases/1.2.0/Bokheim.AppImage
    updates/releases/1.2.0/Bokheim-Windows-x86_64-Update.zip
    updates/releases/1.2.0/Bokheim.apk
    taxonomy/releases/42/snapshot.sqlite3
```

The `origins` configuration maps HTTPS hosts to the `updates` and `taxonomy`
directories. Already published artifacts may be omitted from subsequent bundles;
the publisher rechecks their existing bytes. External packages can instead use
an explicitly configured `external_artifact_prefixes` directory (see the example
configuration for the GitHub release repository). The publisher downloads each
external package anonymously and verifies its signed length and SHA-256 before
promoting the feed, including on retries. It follows HTTPS redirects so release
storage CDNs work. External packages are not copied to the website. Unconfigured
external directories are rejected; the client protocol remains provider independent.

GitHub releases live in `JohanAnderssonOstling/bokheim`, which is private.
Preparation downloads them using the operator's authenticated GitHub CLI, checks
them against successful build artifacts, and puts the verified bytes into the
publication bundle. Clients download public immutable packages from
`https://updates.bokheim.se/releases/<tag>/<filename>` without GitHub credentials.
The source release must be published (not draft) before feed signing, but its
repository may remain private. Publishing a GitHub release alone does not
advertise an in-app update: the signed Bokheim feed is the discovery authority. Keep versioned assets available unchanged
after publication. Changing storage providers only requires different manifest
URLs and the publisher's directory allowlist, with no client code change.

```bash
cargo run --release -p update-client --example sign_manifest -- \
  manifest.json /secure/release-key.pk8 release-1 bundle/manifest.signed.json
cargo run --release -p update-publisher -- \
  config.json bundle /tmp/bokheim-update-host
# Remote publication, from a Linux machine matching the server architecture:
bash servers/updates/deploy-remote.sh deploy@server bundle
```

Use isolated bundle and publication directories, owned exclusively by the
publishing account. The tool rejects symlinks; concurrent hostile filesystem
writers are outside its trust model. Serve only `<root>/public/<namespace>`.
History and lock files remain outside the public tree.

### Feed signing key format

The signer accepts Ed25519 PKCS#8 DER v1 (OpenSSL) and v2 (Ring) keys.
For a new identity, generate the private file on the signing machine with a private
umask; never overwrite an existing signing identity:

```bash
(umask 077; set -o noclobber; openssl genpkey -algorithm ED25519 -outform DER > /secure/new-feed-key.pk8)
```

Existing production signing files on this release machine are kept under
`~/.local/share/bokheim-signing/`; public publisher configuration is under
`~/.config/bokheim-updates/`. The server receives only public trust and signed
manifests. Back up the private signing key through the operator's secure backup
process before distributing clients that trust it.

### Android release signing and verification

APK signing and feed signing use separate keys. Keep the existing distributed
APK's signing key: an unrelated replacement key cannot update those installs.
This pipeline pins a single APK certificate; signing-key rotation is not
implemented. Store an offline backup of the keystore and its recovery details.

The GitHub `android-release` environment uses the signing secrets
`ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`,
`ANDROID_KEY_ALIAS` and `ANDROID_KEY_PASSWORD`. The repository already supplies
these names; the workflow maps them to the Gradle build environment.
The build decodes the keystore into a private temporary file and removes it on
exit. It builds optimized release APKs only; local fixture signing is not enabled.
Set repository variable `BOKHEIM_ANDROID_CERT_SHA256` to the lowercase SHA-256
certificate fingerprint from an existing production APK, independently checked
with Android SDK `apksigner verify --print-certs EXISTING.apk`.

The `verify-android` job needs a Linux x64 runner labeled `bokheim-android`,
Python 3.11+, adb, and a dedicated attached ARM64 Android device. Configure its
serial in repository variable `BOKHEIM_ANDROID_TEST_SERIAL`. The script installs
the signed release and separate instrumentation APK; it does not launch the GUI
or initialize a library. Use a test device without personal data. An incompatible
existing signing identity causes installation to fail; the script never uninstalls
an existing app to bypass that check.

The job reads native compatibility and feed trust from the installed APK, verifies
its production certificate, and binds the receipt to the APK hash. Only that
verified APK enters the draft release. Feed preparation rechecks the pinned
certificate, application identity/version, release mode, hash and shared trust.
The instrumentation APK is retained only as an intermediate CI artifact and is
not distributed in the published release.

### Prepare a release from GitHub builds

The workspace and release workflow supply the configured production public key
(`release-1`). Repository variables `BOKHEIM_UPDATE_PUBLIC_KEY_HEX`,
`BOKHEIM_UPDATE_KEY_ID` and `BOKHEIM_UPDATE_ENDPOINT` can override it for an
explicitly configured release environment.
The AppImage workflow retains the package's actual `--bokheim-update-info` output,
including its bundled trust configuration. An updater-disabled package cannot
pass the preparation gate.

```bash
python3 servers/updates/prepare-github-release.py \
  --source-repository JohanAnderssonOstling/bokheim \
  --release-repository JohanAnderssonOstling/bokheim \
  --run-id BUILD_RUN_ID --tag v1.2.0 --sequence 1 \
  --android-cert-sha256 "$BOKHEIM_ANDROID_CERT_SHA256" \
  --config /secure/bokheim-updates.json --initial --output /tmp/update-bundle
```

Use `--previous-manifest reviewed-previous-payload.json` instead of `--initial`
for later releases; this carries forward the taxonomy offer. Input is the reviewed
plain manifest payload, not the signed envelope. The server still independently
enforces signatures and increasing publication sequence. Validity defaults to
14 days and can be set with `--valid-days` (1–90).
Use `--key-id` when signing with a key other than `release-1`. Every new package
must trust that signing key; the publisher may retain older keys for history.

Preparation requires a successful trusted desktop-release workflow and the latest
successful shared-suite status for its exact source SHA. It downloads the original
workflow artifact and the release asset, compares their bytes, checks the release
source receipt and package compatibility/trust metadata, then writes `manifest.json`
and `provenance.json`, plus the verified packages under `artifacts/`. It never
runs downloaded executables, changes a GitHub release,
signs metadata or updates the live feed. Expired workflow artifacts require a fresh
successful build. GitHub access uses the operator's existing `gh` authentication.

Preparation includes Linux AppImages, Windows update ZIPs and Android ARM64 APKs.
All packages must contain matching version/schema/taxonomy compatibility and
feed trust, proven by packaged runtime metadata. Android also must match the
configured production signing certificate. Browser updates use ordinary website
deployment and are not advertised in the native feed. The command refuses to drop
previously advertised platform targets silently. Native device/GUI rollout trials
remain required before offering a production application update.

Review the manifest and provenance, publish the source GitHub release, then use
the signing/publication command below. Its repository may remain private; the
verified package bytes are included under the bundle's `artifacts/` directory.

### Publish a reviewed preparation

Run this on the trusted release machine (or a protected own runner). Review both
`manifest.json` and `provenance.json`, then record their combined review digest:

```bash
python3 servers/updates/publish-github-release.py \
  --bundle /tmp/update-bundle --print-review-digest
```

Publish the reviewed GitHub release after checking its packages; the repository
can remain private. Publishing
assets does not change the Bokheim update feed. The following command rechecks
the successful source workflow, exact-commit shared test result, published release
and source receipt, signs the exact reviewed bytes, checks the shared gate again,
then invokes the publisher. Package hashes and sizes are checked again before
signing. The publisher verifies the signature and staged artifact bytes before
atomically promoting the feed.

```bash
python3 servers/updates/publish-github-release.py \
  --bundle /tmp/update-bundle --review-sha256 RECORDED_REVIEW_DIGEST \
  --config /secure/bokheim-updates.json --key /secure/release-key.pk8 \
  --output /tmp/signed-update-bundle --host publisher@server
```

Use `--staging-root /tmp/bokheim-update-staging` instead of `--host` to run the
same publisher against a local directory first. This requires authenticated
access to the published source release. For remote publication, the command also fetches the public feed
and requires it to match the signed envelope exactly. Signing keys stay local;
only the signed bundle and publisher binary are transferred.

Signed output is retained if publication or public verification fails. A network
error may happen after promotion, so inspect the public feed before retrying.
Rerunning with the same reviewed inputs and a fresh output directory rechecks
the gates and recreates the same signed payload; the publisher is idempotent.
Changes to either reviewed JSON file require a new review digest. This integration
handles prepared application releases and their carried-forward taxonomy offer;
the generic publisher/signing tools remain available for independent taxonomy
publication and other artifact providers.

The provider integration uses GitHub's documented
[workflow run API](https://docs.github.com/en/rest/actions/workflow-runs#get-a-workflow-run)
and [release download command](https://cli.github.com/manual/gh_release_download).

## One-time production setup (not performed)

- Set DNS for `updates.bokheim.se` to the Caddy host and permit HTTPS certificate
  issuance. The existing taxonomy host serves immutable `/releases/*` as well
  as its existing API.
- Create `/srv/bokheim-updates`, owned by the dedicated SSH publishing account,
  traversable/readable by Caddy. Publication must use a `022` umask so new
  directories are readable by the web server. Give this account no sudo rights.
- Install a root-owned `/etc/bokheim-updates.json` from `config.example.json`,
  replacing the placeholder with the real public key. Never copy the private key.
  After reviewing that config, use an optimized publisher binary to validate and
  provision the directory and config on the host:

  ```bash
  cargo build --release --locked -p update-publisher
  target/release/update-publisher --check-config /secure/bokheim-updates.json
  # On the serving host, using a copied PUBLIC config and release binary:
  sudo bash provision-publisher.sh publisher public-config.json ./update-publisher
  ```

  The account must already exist and be non-root. The provisioning script grants
  no sudo rights, installs no credentials, and changes neither Caddy nor the feed.
  It refuses to replace differing installed trust or take over a publication root
  owned by another account. Key rotation needs its own reviewed configuration change.
- Bundle that same public key and key ID into application builds using
  `BOKHEIM_UPDATE_PUBLIC_KEY_HEX` / `BOKHEIM_UPDATE_KEY_ID`.
- Validate and deploy the updated `servers/sync/deploy/Caddyfile` using the
  existing Caddy configuration deployment procedure. Existing server deployments
  also copy this shared file, so review the new host before deploying them.
- Configure protected release signing and publication credentials in CI if
  desired. Existing desktop/Kobo GitHub workflows still produce draft releases;
  they do **not** automatically advertise an update. The deployment script works
  identically from an approved CI job or a release machine.

## Failures and operations

- Failed copy, signature, checksum, immutable collision or stale sequence leaves
  the previous feed in place. Retry the same bundle safely. Unreferenced immutable
  files from an interrupted attempt may remain; they are not update offers.
- An interruption after manifest replacement may have committed successfully.
  Rerunning the same bundle is idempotent. Different metadata needs a new sequence.
- Never overwrite versioned URLs. Publish corrected packages under new versions
  or revision paths, with a higher manifest sequence.
- To withdraw an offer, sign a higher-sequence manifest omitting that component.
  Already approved client plans remain pinned; withdrawal is not remote revocation.
- Renew the signed manifest before expiry with a higher sequence even if its
  artifacts are unchanged. Expired metadata produces no new client offer. Retain
  old public keys when rotating so the publisher can verify previous history.
- Retain old artifacts: clients may have approved an earlier release and resume
  it later. There is no automatic artifact garbage collection yet.
- Monitor feed expiration, HTTPS availability, publish failures and disk space.
  These are operator concerns, not recoverable-error prompts in the app.

## Existing backend deployments

Sync, metadata and taxonomy **server code** retain their existing deployment
scripts and health checks. Publishing a client update does not restart those
services. Deploy compatible API additions first; publish clients second; remove
old API behavior only after the supported-client policy allows it.

The existing taxonomy deployment helper stamps a new release ID during deployment.
Archive and hash the **final deployed snapshot**, not the unstamped source file.
Publishing validates bytes and signatures; semantic taxonomy checks and migration
tests are release gates that must run before signing. This tool does not run SQL
from a manifest or modify users' libraries.

See `IMPLEMENTATION.md` for current deployment and verification status. Hosting
and an initial signed feed are live; that feed advertises no application updates.
Packaged rollout trials and the first updater-enabled distribution remain pending.
