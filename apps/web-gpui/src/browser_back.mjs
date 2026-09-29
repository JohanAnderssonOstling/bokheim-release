// Browser Back is an app dismissal gesture, matching native Android. These
// entries are a guard, not a route stack; Forward does not undo dismissals.
const stateKey = '__bokheimBack';

export function installBrowserBack(onBack) {
    const existing = history.state?.[stateKey];
    const token = existing?.token ?? crypto.randomUUID();
    let armed = existing?.kind === 'guard';
    let leaving = false;
    let restoring = false;

    function pushGuard() {
        history.pushState({...history.state, [stateKey]: {token, kind: 'guard'}}, '');
        armed = true;
    }

    function arm() {
        if (armed || leaving || restoring) return;
        history.replaceState({...history.state, [stateKey]: {token, kind: 'base'}}, '');
        pushGuard();
    }

    // Wait for interaction so browsers do not treat startup-created history
    // entries as an attempt to prevent an immediate departure from the page.
    window.addEventListener('pointerdown', arm, {capture: true});
    window.addEventListener('keydown', arm, {capture: true});

    window.addEventListener('popstate', event => {
        const entry = event.state?.[stateKey];
        if (entry?.token !== token) {
            armed = false;
            restoring = false;
            return;
        }
        if (entry.kind === 'guard') {
            // Forward into the guard must not dispatch another Back action.
            armed = true;
            restoring = false;
            return;
        }
        if (!armed || leaving || entry.kind !== 'base') return;
        armed = false;
        let handled = false;
        try {
            handled = onBack();
        } catch (error) {
            console.error('Bokheim Back handler failed', error);
        }
        if (handled) {
            // Reuse the guard that Back just left. Pushing a replacement here
            // would make Chromium skip this document on the next toolbar Back
            // unless the page received another user activation in between.
            restoring = true;
            history.forward();
        } else {
            // GPUI propagated Back all the way out, just as Android finishes
            // its activity. Skip our base entry to reach the preceding page.
            leaving = true;
            history.back();
            // A directly opened tab may have no preceding page. Browsers
            // cannot close it for us; permit later interaction to rearm Back.
            setTimeout(() => { leaving = false; }, 250);
        }
    });

    window.addEventListener('pagehide', () => { armed = false; restoring = false; });
    window.addEventListener('pageshow', () => {
        // A BFCache restore or reload must neither stack another guard nor
        // interpret returning to the base entry as a new Back gesture.
        const entry = history.state?.[stateKey];
        armed = entry?.token === token && entry.kind === 'guard';
        restoring = false;
        leaving = false;
    });
}
