# Kobo 0.1.10 installer inputs

The Windows and Linux installers embed `Bokheim-Kobo-armv7.tar.gz`, the locally
built generic ARMv7 Kobo application from 2026-10-24. `manifest.json` records its
version, size, SHA-256 and build timestamp. The installer workflow verifies the
archive, embedded version, supported models, ARM binaries and runtime before
building either desktop installer. Both platforms run the installer tests.

This branch packages that existing application archive. It does not rebuild the
ARM application or attest a new application source revision. The branch commit
identifies the installer and packaging source; the manifest identifies the
embedded application bytes.

Download and verify both completed Actions artifacts before adding the missing
Kobo assets to the published `v0.1.10` release. Preserve existing release assets.
Publish the archive under both `Bokheim-Kobo-armv7.tar.gz` and the historical
`Bokheim-Kobo-Libra2-armv7.tar.gz` name so existing readers can find updates.
