// The reader stays cross-origin isolated. Google's popup runs from a separate
// same-origin page, and returns its grant through a random BroadcastChannel.
const scopes = ['openid', 'https://www.googleapis.com/auth/userinfo.email', 'https://www.googleapis.com/auth/drive'];
const lifetime = 300000;

function failure(message) { return Error(message); }
function validClientId(clientId) { return typeof clientId === 'string' && clientId.length > 0 && clientId.length <= 4096 && !/\s/.test(clientId); }

export function validateGoogleGrant(grant) {
    if (!grant || typeof grant.access_token !== 'string' || !grant.access_token.length || grant.access_token.length > 32768 || /[\s\x00-\x1f\x7f]/.test(grant.access_token) ||
        !Number.isInteger(grant.expires_in_seconds) || grant.expires_in_seconds <= 0 || grant.expires_in_seconds > 86400)
        throw failure('Google returned an invalid access grant.');
    return {access_token: grant.access_token, expires_in_seconds: grant.expires_in_seconds};
}

export function prepareGoogleLogin(clientId) {
    if (!validClientId(clientId)) throw failure('Google Drive browser sign-in is not configured.');
    if (typeof BroadcastChannel !== 'function') throw failure('This browser cannot connect Google Drive.');
    return {clientId, page: new URL('google_cloud_login.html', document.baseURI).href};
}

export function beginGoogleLogin(config) {
    const nonce = crypto.randomUUID();
    const channel = new BroadcastChannel(`bokheim-google-${nonce}`);
    const target = new URL(config.page);
    target.hash = new URLSearchParams({channel: nonce, client: config.clientId}).toString();
    let finish, settled = false;
    const onExit = () => finish(failure('Google sign-in was closed.'));
    const result = new Promise((resolve, reject) => {
        const timer = setTimeout(() => finish(failure('Google sign-in timed out. Connect again.')), lifetime);
        finish = (error, grant) => {
            if (settled) return;
            settled = true;
            clearTimeout(timer);
            window.removeEventListener('pagehide', onExit);
            channel.postMessage({type: 'cancel'});
            channel.close();
            if (error) reject(error); else resolve(grant);
        };
        channel.onmessage = ({data}) => {
            if (data?.type === 'ready') { channel.postMessage({type: 'ack'}); return; }
            if (data?.type === 'grant') {
                try { finish(null, validateGoogleGrant(data.grant)); }
                catch { finish(failure('Google returned an invalid access grant.')); }
            } else if (data?.type === 'closed') finish(failure('Google sign-in was closed.'));
            else if (data?.type === 'error') finish(failure('Could not connect Google Drive. Try again.'));
        };
    });
    // Cancellation may happen before Rust starts awaiting this promise.
    result.catch(() => {});
    window.addEventListener('pagehide', onExit, {once: true});
    const attempt = {result, cancel: () => finish(failure('Google sign-in was closed.'))};
    try {
        if (!window.open(target.href, '_blank', 'popup,width=520,height=650')) finish(failure('Allow popups to connect Google Drive.'));
    } catch { finish(failure('Could not open Google sign-in.')); }
    return attempt;
}

export function googleLoginResult(attempt) { return attempt.result; }
export function cancelGoogleLogin(attempt) { attempt.cancel(); }

function loadGoogle() {
    if (globalThis.google?.accounts?.oauth2) return Promise.resolve();
    return new Promise((resolve, reject) => {
        const script = document.createElement('script');
        const timer = setTimeout(() => finish(failure('Google sign-in did not load. Try again.')), 15000);
        const finish = error => {
            clearTimeout(timer);
            script.onload = script.onerror = null;
            if (error) { script.remove(); reject(error); } else resolve();
        };
        script.src = 'https://accounts.google.com/gsi/client';
        script.async = true;
        script.onload = () => globalThis.google?.accounts?.oauth2 ? finish() : finish(failure('Google sign-in is unavailable.'));
        script.onerror = () => finish(failure('Could not load Google sign-in. Check your connection.'));
        document.head.append(script);
    });
}

export async function runGoogleLoginPage() {
    const button = document.querySelector('#connect');
    const cancel = document.querySelector('#cancel');
    const status = document.querySelector('#status');
    const params = new URLSearchParams(location.hash.slice(1));
    const nonce = params.get('channel');
    const clientId = params.get('client');
    history.replaceState(null, '', location.pathname);
    if (!/^[0-9a-f-]{36}$/.test(nonce || '') || !validClientId(clientId)) {
        status.textContent = 'Open Google Drive sign-in from Bokheim.';
        return;
    }
    const channel = new BroadcastChannel(`bokheim-google-${nonce}`);
    let settled = false, acknowledged = false, loaded = false;
    const timer = setTimeout(() => finish({type: 'closed'}, 'Google sign-in timed out. Return to Bokheim and connect again.'), lifetime);
    const finish = (message, text) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        button.disabled = true;
        if (message) channel.postMessage(message);
        channel.close();
        status.textContent = text;
    };
    channel.onmessage = ({data}) => {
        if (data?.type === 'ack') { acknowledged = true; button.disabled = !loaded; }
        if (data?.type === 'cancel') { finish(null, 'Sign-in closed. You can close this window.'); window.close(); }
    };
    const onExit = () => { if (!settled) channel.postMessage({type: 'closed'}); };
    window.addEventListener('pagehide', onExit, {once: true});
    cancel.onclick = () => { finish({type: 'closed'}, 'Sign-in closed.'); window.close(); };
    channel.postMessage({type: 'ready'});
    try {
        await loadGoogle();
        if (settled) return;
        loaded = true;
        button.disabled = !acknowledged;
        status.textContent = 'Choose your Google account and allow access to your library folder.';
        button.onclick = () => {
            if (!acknowledged || settled || button.disabled) return;
            button.disabled = true;
            status.textContent = 'Waiting for Google…';
            try {
                const oauth = google.accounts.oauth2;
                const client = oauth.initTokenClient({
                    client_id: clientId, scope: scopes.join(' '), include_granted_scopes: false, prompt: 'select_account',
                    callback: response => {
                        if (settled) return;
                        try {
                            if (response?.error || !oauth.hasGrantedAllScopes(response, ...scopes) || !/^bearer$/i.test(response.token_type || ''))
                                throw failure('Allow Google Drive and account access to connect your library.');
                            const grant = validateGoogleGrant({access_token: response.access_token, expires_in_seconds: Number(response.expires_in)});
                            finish({type: 'grant', grant}, 'Google Drive connected. Return to Bokheim.');
                            window.close();
                        } catch (error) {
                            status.textContent = error.message;
                            button.disabled = false;
                        }
                    },
                    error_callback: error => {
                        if (settled) return;
                        status.textContent = error?.type === 'popup_failed_to_open' ? 'Allow popups, then try again.' : 'Google sign-in was closed. You can try again.';
                        button.disabled = false;
                    },
                });
                client.requestAccessToken();
            } catch { status.textContent = 'Could not open Google sign-in. Try again.'; button.disabled = false; }
        };
    } catch (error) {
        if (!settled) finish({type: 'error'}, error.message);
    }
}
