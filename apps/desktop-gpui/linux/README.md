# Linux packaging

## AppImage

The release workflow builds an x86-64 AppImage and its SHA-256 checksum.
GitHub can store these artifacts, while in-app update discovery uses the signed
Bokheim feed. The package does not embed a separate zsync update endpoint.

Build the release binary and package it with versioned releases of
`linuxdeploy` and `appimagetool` available on `PATH`:

```sh
python3 apps/desktop-gpui/scripts/build.py
apps/desktop-gpui/linux/appimage/build-appimage.sh
```

The packaging script writes `Bokheim-x86_64.AppImage` and
its `.sha256` file to `release/`. Set `RELEASE_DIRECTORY`, `DESKTOP_BINARY`,
`LINUXDEPLOY`, or `APPIMAGETOOL` to override the defaults.
The release workflow also inspects the AppImage runtime and every packaged ELF
file and rejects an artifact that requires a glibc version newer than 2.35.

Update discovery is enabled only when the build contains a valid public signing
key. Settings shows one row when an update is available. Explicit Update consent
persists approval and stages verified packages; activation waits for a later
launch. Restart is an optional explicit action. Database/taxonomy changes and the
AppImage replacement use a recoverable startup transaction.

The `APPIMAGE` environment variable identifies the package that can be replaced.
Without it, the host can offer compatible taxonomy updates only. Package launchers
can also disable application replacement using `BOKHEIM_DISABLE_SELF_UPDATE=1`;
the AUR package must set this variable. Installation permissions and actual storage
deficits appear only when action is required. See `servers/updates/README.md` for
public-key configuration, release preparation and signed feed publication.

The GitHub workflow checks out the companion GPUI-Fork, HtmlEngine, and
HtmlViewCore repositories beside this repository because the Cargo workspace
currently uses sibling paths. Their pinned revisions must be updated whenever
Bokheim starts depending on newer companion commits.

## Flatpak

Flatpak builds use the `flatpak` Cargo feature. It keeps PDFium out of the
executable and loads the application-private library installed at
`/app/lib/libpdfium.so`.

The Flatpak manifest must fetch the pinned PDFium archive declared in
`shared/pdfium/artifacts.toml`, verify its SHA-256 checksum, and make the
archive available to Cargo without network access:

```sh
python3 shared/pdfium/prepare.py --target x86_64-unknown-linux-gnu \
  --archive /run/build/bokheim/pdfium-linux-x64.tgz --offline
cargo build --locked --release -p desktop-gpui --features flatpak
```

Install the artifacts into the Flatpak application prefix:

```text
/app/bin/desktop-gpui
/app/lib/libpdfium.so  # from .pdfium/x86_64-unknown-linux-gnu/
/app/share/applications/se.bokheim.Bokheim.desktop
```

`PDFIUM_PACKAGED_PATH` can override the compile-time package location when a
different immutable application prefix is required. For local diagnostics,
`BOKHEIM_PDFIUM_LIBRARY_PATH` overrides the library at runtime.
