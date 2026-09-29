# Windows distribution and updates

The release workflow produces two packages from the same optimized build:

- `Bokheim-Windows-x86_64-Setup.exe`: initial per-user installation with shortcuts
  and uninstall registration.
- `Bokheim-Windows-x86_64-Update.zip`: complete runtime payload for the signed
  update mechanism. Contains `Bokheim.exe` and `Bokheim.ico`.

PDFium 8046b and the C runtime are statically linked into the executable.
There is no PDFium DLL to stage, load, replace or restore separately. Build it
with `package-update.py`, pointing `--binary` to the optimized release executable,
`--icon` to the application icon and `--output` to the intended ZIP path. Packaging
requires Windows PE headers for the executable and writes a portable SHA-256 sidecar. The signed feed must authenticate
the ZIP's full length and hash; the sidecar is not a trust source.

`update-client::windows_package` inspects and expands an authenticated ZIP into a
new private candidate directory. It rejects path traversal, alternate data streams,
DOS device names, links, case-insensitive duplicates and file/directory collisions.
The exact declared expanded bytes are exposed for storage accounting; extraction
enforces entry lengths and ZIP checksums. The host must account separately for
backups and database migration workspace, then journal every live replacement.

The startup helper waits for the original process to exit, then promotes staged
files under desktop ownership. A launch lock prevents another normal startup
from racing the swap; only the helper's specifically authorized child may enter.
A durable journal covers executable, taxonomy and database changes. Failed trial
startup (including failure to create the process) restores backups automatically.
Committed updates are never rolled back. Obsolete locked files are retried during
later cleanup. The Settings row uses the shared Update/Restart controls.

Windows MSVC release checks and real executable helper trials pass under Wine:
successful activation and crashing-new-version rollback, including registry
restoration. Windows CI runs the process fixture and verifies compatibility/trust
metadata by executing the actual ZIP payload. Release preparation includes the
Windows ZIP alongside Linux. Native Windows GUI trials are still required before
production rollout.

## Linux cross checks

GPUI's Windows build script currently skips shader compilation on Linux. After
Cargo reports its missing `shaders_bytes.rs`, prepare the reported OUT_DIR:

```sh
WINEPREFIX=/path/to/wine-prefix python3 apps/desktop-gpui/windows/prepare-cross-shaders.py --out-dir /path/from/cargo/out
RC_x86_64_pc_windows_msvc=x86_64-w64-mingw32-windres cargo xwin check --release -p desktop-gpui --target x86_64-pc-windows-msvc
```

This requires MinGW GCC, Wine and cargo-xwin. The script invokes D3DCompile with
optimization level 3 for all 18 entry points, validates DXBC output and records
source/output hashes. It does not substitute dummy shaders. Native Windows CI
continues to use GPUI's normal Windows SDK fxc compiler.
