# Release test ownership

Prefer the local workstation for shared tests and trusted Linux/web/Android
checks and Android builds. Windows and distribution-specific packaging use
GitHub-hosted runners. Pull-request platform checks stay on disposable hosted
runners; shared tests run only by explicit dispatch or local invocation.

| Suite | Execution |
| --- | --- |
| Library schema/replica/backend, taxonomy, shared services and runtime | Own Linux runner |
| Update coordinator, native staging and publisher | Own Linux runner |
| Kobo extraction rules and platform-neutral Android/Kobo adapter logic | Own Linux runner |
| PostgreSQL synchronization E2E | Own Linux runner |
| Linux update activation/recovery subprocess test | Local workstation |
| Native adapter tests | Local Linux and GitHub Windows |
| Web worker/Wasm checks and browser runtime helpers | Local workstation |
| Android target cross-compilation and release APK | Local workstation |
| AppImage payload/linking and Windows installer checks | GitHub respective OS |
| Kobo installer package checks and OS path conventions | GitHub respective OS |

All Rust builds/tests/checks use optimized release mode. Compiling shared code as
part of a platform build is expected; its test suite is not rerun on each OS.
The old architecture check scripts no longer exist in this checkout and are no
longer referenced by workflows.

## Run shared tests

```bash
python3 scripts/ci/shared-tests.py --dry-run
python3 scripts/ci/shared-tests.py
# After tests on a clean release checkout, report the result to the source repo:
python3 scripts/ci/shared-tests.py --report OWNER/REPO
```

`--report` runs the suite, posts pending before execution and success only after
all commands pass. It requires the main checkout to remain clean at the same
commit, and all three sibling repositories to match the desktop release pins.
A dirty development checkout may run tests without reporting release readiness.
The status protocol is `bokheim/shared-tests-v1`; changes that invalidate old
results should bump this context in both scripts.

Local reporting requires `gh` authentication with commit-status write permission.
Only trusted release operators/CI should have that permission. The release gate
checks the newest status for this context and exact checkout SHA; repository
status-writing authority is the trust boundary. It never accepts an earlier
success over a later failure or pending result. GitHub's commit-status protocol:
https://docs.github.com/en/rest/commits/statuses

Alternatively dispatch **Shared release tests (own runner)** on the release
branch/tag. The workflow is manual and runs on labels
`self-hosted, linux, x64, bokheim-shared`, not a GitHub-hosted runner. Provision it
with Rust 1.95.0, Python 3, gh, ripgrep, PostgreSQL tools, and the Linux
native build libraries used by the desktop/Kobo packages. Do not automatically
run untrusted pull requests on this runner. A local run without a supplied
`SYNC_E2E_DATABASE_URL` needs PostgreSQL's initdb/pg_ctl/createdb in PATH.

The shared workflow uses the same native PostgreSQL lifecycle as local tests: it
starts an isolated temporary database on an available port and stops it afterwards.
Docker and a fixed PostgreSQL service port are not required. GitHub authentication
and Actions runner `johan-82sn-bokheim` are configured on the release workstation.
Its labels are `bokheim-shared` and `bokheim-local`; its isolated work directory
is `~/.local/share/bokheim-actions-runner/_work`. The user service
`bokheim-actions-runner.service` starts with the user session and can be controlled
with `systemctl --user`. Rust 1.95.0 is installed without changing the developer
default toolchain. Android device verification needs an attached device and the
`bokheim-android` label; that label is not assigned yet. The local reporting command works without a runner daemon.

## Independent platform runs

Run one platform per invocation against the same source ref:

```bash
gh workflow run desktop-release.yml --repo JohanAnderssonOstling/bokheim-release --ref main -f platform=linux -f publish=false
gh workflow run desktop-release.yml --repo JohanAnderssonOstling/bokheim-release --ref main -f platform=windows -f publish=false
gh workflow run desktop-release.yml --repo JohanAnderssonOstling/bokheim-release --ref main -f platform=android -f publish=false
gh workflow run client-platforms.yml --repo JohanAnderssonOstling/bokheim-release --ref main -f platform=web
```

Use an existing immutable release tag when all runs must use an identical ref.
Each invocation has its own run and can be retried independently. Desktop runs
reuse the newest successful shared-test status for their exact commit; otherwise
the local shared suite runs first. A later failure or pending status invalidates
an earlier success. Browser checks/builds run separately from executable packaging.
Local jobs share one runner and therefore execute sequentially on that machine.
Local platform Cargo artifacts live outside the cleaned checkout; hosted platform
jobs save and restore their Cargo cache.

## Release gate

1. Shared tests must pass for the exact release commit.
2. Each executable requires only its own platform checks before packaging.
3. A selected platform can upload its verified artifacts to the shared draft with
   `publish=true`; other platform jobs are skipped. `platform=all` is available
   for a combined run, and existing tag pushes retain combined release behavior.
4. Publication checks the shared result again and requires an existing version
   tag pointing to the exact source commit. It never creates or moves tags.
5. Draft assets are uploaded to `JohanAnderssonOstling/bokheim-release` using the
   repository Actions token with write access only in the publication job.
6. Artifacts include a source-commit receipt. Drafts remain unpublished, and public
   releases cannot be overwritten by these jobs. All desired platform assets must
   be collected and verified before publishing the draft publicly.

These gates apply to existing **draft GitHub release uploads**. The reviewed
`servers/updates/publish-github-release.py` command rechecks the source workflow,
published release/source receipt and exact-commit shared gate before signing, and
checks the shared gate again before publication. Run it on a trusted release
machine or protected own runner; private keys never reach the serving host.
See `servers/updates/README.md` for review, local staging and publication commands.
Direct operator use of the static publisher remains a trusted release operation.

Android APK release signing/emulator installation tests and Windows next-launch
activation tests remain pending. Linux now has a subprocess activation/recovery
test and an AppImage update-metadata entry-point check. The existing
Windows installer check validates its PE container; it does not claim a complete
install/restart exercise. Add those tests to the relevant platform jobs as the
activation adapters become available.
