# Browser updates

Browser updates are automatic on page load. The HTML shell revalidates and selects
an immutable WASM/JavaScript bundle. Existing tabs keep their running bundle.
The coordinator and dedicated workers use that same bundle URL.

The database worker owns the exclusive storage lock. A new bundle cannot migrate
while an old worker owns the database; the page asks the user to close other tabs.
Before exposing startup state, the backend migrates every registered library and
reprojects assignments using the taxonomy bundled with the deployed code.
Schema and taxonomy work use the existing database transactions and stored versions.
Background work and object reclamation start only after successful initialization.

The page waits for actual backend/UI readiness. A startup failure reloads once per
tab, then shows a Reload button. A blocked storage owner shows the close-other-tabs
message immediately. Success clears the retry marker. Active tabs are not forced
to reload. Migration readiness has no fixed time limit: larger libraries must be
allowed to finish. Worker and transport failures still report startup errors.

There is no browser approval, staged bundle selection, custom artifact cache,
database snapshot, rollback journal or separate browser update feed. Website
publication delivers code and its taxonomy together. Native update consent and
signed distribution remain separate.

Verification:

    node apps/web-gpui/scripts/startup.browser-test.mjs

PLAYWRIGHT_MODULE and FIREFOX_PATH can select local Playwright/Firefox installs.

After building an optimized release, exercise the actual application in Chromium:

    BOKHEIM_TEST_DIST=/path/to/release node apps/web-gpui/scripts/deployment.browser-test.mjs

This serves the same compiled application under two immutable deployment identities
and checks slow startup, older-tab ownership, new-bundle selection and startup
failure recovery. PLAYWRIGHT_MODULE and CHROMIUM_PATH select local installations.
WASM_BINDGEN and CARGO_TARGET_DIR can select local build tools/artifacts.
