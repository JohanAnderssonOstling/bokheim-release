# Bokheim on the Arch User Repository

This directory contains the `bokheim-bin` packaging recipe for the public x86-64
Linux release. It verifies the versioned AppImage's SHA-256 checksum, extracts
the application into `/opt/bokheim`, and installs `/usr/bin/bokheim`, the desktop
launcher, and icons. FUSE is not needed at runtime. WebKitGTK and the other host
dependencies are installed through pacman.

## Before publication

Bokheim is licensed under GPL-3.0-or-later. The repository's root `LICENSE`
contains the full GPL text and `COPYRIGHT` contains the application license
notice. This directory's `COPYING` and `COPYRIGHT` are copies of those files,
included as local AUR sources so packaging does not depend on an unversioned
download. Keep the copies and their checksums synchronized with the originals.
The package installs them in `/usr/share/licenses/bokheim-bin/` as `LICENSE`
and `COPYRIGHT`.

The `LICENSE` in this directory covers only the packaging recipe and launcher,
under 0BSD, and is installed as `PACKAGING-LICENSE`. Third-party licenses and
notices bundled in the AppImage remain in the extracted installation.

The native speech build statically links eSpeak NG through
`vendor/sherpa-onnx-sys/build.rs`. eSpeak NG is
[GPL-3.0-or-later](https://github.com/espeak-ng/espeak-ng#license-information),
so the combined application distribution needs to satisfy those GPL terms.
The application uses GPL-3.0-or-later accordingly. This is a targeted dependency
check, not a complete review of every bundled library, font, and voice model.

The maintainer contact is Johan <johan@bokheim.se>. Keep `bokheim-bin` for these
prebuilt releases and make the corresponding application source available
before publication. The source and build instructions need to cover the
companion GPUI-Fork, HtmlEngine, and HtmlViewCore revisions used by the release.
Check for existing `bokheim` and `bokheim-bin` packages before creating a new AUR
repository.

## Build and install locally

On an updated Arch Linux installation, install the `base-devel` package,
then run from this directory as a regular user:

```sh
makepkg --verifysource
makepkg --syncdeps
```

Inspect the package and optionally install it:

```sh
bsdtar -tf bokheim-bin-*.pkg.tar.zst
namcap PKGBUILD bokheim-bin-*.pkg.tar.zst
sudo pacman -U ./bokheim-bin-*.pkg.tar.zst
bokheim
```

`namcap` is a separate optional validation tool. Test launching on Wayland and
X11, selecting and reopening libraries, PDF rendering, audiobook playback,
text-to-speech, and the embedded browser. A working desktop portal backend is
needed for native file dialogs; use the backend appropriate to the desktop.

The package launcher disables replacement of application binaries and clears
inherited AppImage runtime variables. Update the application through the AUR
package. Compatible taxonomy updates remain available inside Bokheim. The
installed app uses the normal desktop data location, shared with an existing
AppImage installation; back up important libraries before switching versions.

## Publish the recipe

After making the corresponding sources available and testing, create an AUR
account and register an SSH public key. Clone the package repository in a
separate directory:

```sh
git clone ssh://aur@aur.archlinux.org/bokheim-bin.git
```

Copy `PKGBUILD`, `bokheim`, `LICENSE`, `COPYING`, and `COPYRIGHT` from this
directory into that checkout. Generate the metadata there, then commit and
push the recipe files:

```sh
makepkg --printsrcinfo > .SRCINFO
git add PKGBUILD .SRCINFO bokheim LICENSE COPYING COPYRIGHT
git commit -m "Add Bokheim binary package"
git push origin HEAD:master
```

Upload only the recipe, metadata, launcher, and license notices. Binaries and
generated package archives stay on the upstream release host, outside AUR.

## Update a release

1. Publish the versioned Linux AppImage upstream.
2. Update `pkgver`, reset `pkgrel` to `1`, and replace the AppImage checksum with
   the verified checksum of that release. Never use a `latest` download URL.
3. Keep `COPYING` and `COPYRIGHT` synchronized with the repository's root license
   and copyright notice. Update the checksum of each local source that changes.
4. Run the source verification, build, and runtime checks above.
5. Regenerate `.SRCINFO`, commit the changed recipe files, and push to AUR.

For packaging-only fixes, increment `pkgrel` instead of changing `pkgver`.

References: [AUR submission guidelines](https://wiki.archlinux.org/title/AUR_submission_guidelines)
and [Arch package guidelines](https://wiki.archlinux.org/title/Arch_package_guidelines).
