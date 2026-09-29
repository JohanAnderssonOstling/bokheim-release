// Keep credential fields in the ordinary DOM, with stable form/field identities.
// No credential values are mirrored into application state or browser storage.
export function openAuthentication(themeJson, minimum, submit) {
    const theme = JSON.parse(themeJson);
    const previousFocus = document.activeElement;
    const dialog = document.createElement('dialog');
    dialog.id = 'bokheim-authentication';
    dialog.setAttribute('aria-labelledby', 'bokheim-auth-title');
    for (const [name, value] of Object.entries(theme)) dialog.style.setProperty(`--auth-${name}`, value);
    const style = document.createElement('style');
    style.textContent = `
        #bokheim-authentication { box-sizing:border-box; width:min(440px,calc(100% - 32px)); max-height:calc(100dvh - 32px); overflow:auto; margin:auto; padding:24px; border:1px solid var(--auth-border); border-radius:0; background:var(--auth-background); color:var(--auth-text); font:16px/1.5 "Libertinus Serif",Georgia,serif; }
        #bokheim-authentication::backdrop { background:rgb(0 0 0 / 40%); }
        #bokheim-authentication * { box-sizing:border-box; }
        #bokheim-authentication h2 { margin:0 0 20px; font-size:24px; line-height:1.3; padding-right:44px; }
        #bokheim-authentication form { display:flex; flex-direction:column; gap:16px; }
        #bokheim-authentication label { display:block; margin-bottom:6px; }
        #bokheim-authentication input { display:block; width:100%; min-width:0; min-height:44px; padding:10px 12px; border:1px solid var(--auth-border); border-radius:0; background:var(--auth-background); color:var(--auth-text); font:inherit; }
        #bokheim-authentication button { min-height:44px; padding:8px 12px; border:1px solid var(--auth-border); border-radius:0; background:var(--auth-background); color:var(--auth-text); font:inherit; cursor:pointer; }
        #bokheim-authentication button[type=submit] { background:var(--auth-accent); color:var(--auth-accentText); border-color:var(--auth-accent); }
        #bokheim-authentication :focus-visible { outline:2px solid var(--auth-accent); outline-offset:3px; }
        #bokheim-authentication button:disabled { opacity:.6; cursor:wait; }
        #bokheim-authentication .auth-links { display:flex; flex-direction:column; gap:6px; margin-top:16px; }
        #bokheim-authentication .auth-links button { border-color:transparent; color:var(--auth-textAccent); text-decoration:underline; }
        #bokheim-authentication .auth-actions { display:grid; grid-template-columns:1fr 1fr; gap:12px; }
        #bokheim-authentication .auth-actions button { min-width:0; }
        #bokheim-authentication .auth-secret { position:relative; }
        #bokheim-authentication .auth-secret input { padding-right:52px; }
        #bokheim-authentication .auth-reveal { position:absolute; right:1px; top:50%; transform:translateY(-50%); width:44px; height:44px; padding:0; border:0; background:transparent; display:grid; place-items:center; }
        #bokheim-authentication .auth-reveal svg { width:20px; height:20px; pointer-events:none; }
        #bokheim-authentication .auth-close { position:absolute; top:12px; right:12px; width:44px; height:44px; padding:0; display:grid; place-items:center; font-size:22px; }
        #bokheim-authentication p { margin:0; }
        #bokheim-authentication .auth-hint { color:var(--auth-muted); font-size:14px; margin-top:6px; }
        #bokheim-authentication [role=alert]:empty, #bokheim-authentication [role=status]:empty { display:none; }
        #bokheim-authentication [role=alert] { border-left:3px solid var(--auth-accent); padding-left:10px; }
    `;
    const heading = document.createElement('h2');
    heading.id = 'bokheim-auth-title';
    const content = document.createElement('div');
    const close = button('×', () => destroy());
    close.className = 'auth-close';
    close.setAttribute('aria-label', 'Close sign in');
    dialog.append(style, heading, close, content);
    let flow = 'login', busy = false, closed = false;
    let form, links, error, status;
    const panel = {destroy};
    function destroy() {
        if (closed) return;
        closed = true;
        globalThis.removeEventListener('pagehide', destroy);
        globalThis.removeEventListener('popstate', destroy);
        dialog.close();
        dialog.remove();
        panel.destroy = () => {}; // Release the detached form and its field values.
        submit = null;
        // The browser's credential form is gone before canvas focus returns.
        if (previousFocus?.isConnected) previousFocus.focus({preventScroll: true});
    }
    function button(text, action) {
        const element = document.createElement('button');
        element.type = 'button';
        element.textContent = text;
        element.addEventListener('click', action);
        return element;
    }
    function field(name, label, type, autocomplete, value = '') {
        const group = document.createElement('div');
        const input = document.createElement('input');
        input.id = `bokheim-${flow}-${name}`;
        input.name = name;
        input.type = type;
        input.autocomplete = autocomplete;
        input.required = true;
        input.value = value;
        const title = document.createElement('label');
        title.htmlFor = input.id;
        title.textContent = label;
        group.append(title);
        if (type === 'email') {
            input.autocapitalize = 'none';
            input.spellcheck = false;
        }
        if (type === 'password') {
            const row = document.createElement('div');
            row.className = 'auth-secret';
            const reveal = button('', () => {
                const visible = input.type === 'password';
                input.type = visible ? 'text' : 'password';
                slash.style.display = visible ? '' : 'none';
                reveal.title = visible ? 'Hide password' : 'Show password';
                reveal.setAttribute('aria-pressed', String(visible));
                reveal.setAttribute('aria-label', visible ? 'Hide password' : 'Show password');
            });
            reveal.className = 'auth-reveal';
            reveal.title = 'Show password';
            reveal.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true" focusable="false"><path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12Z"/><circle cx="12" cy="12" r="3"/><path class="auth-eye-slash" d="m3 3 18 18"/></svg>';
            const slash = reveal.querySelector('.auth-eye-slash');
            slash.style.display = 'none';
            reveal.setAttribute('aria-label', 'Show password');
            reveal.setAttribute('aria-controls', input.id);
            reveal.setAttribute('aria-pressed', 'false');
            row.append(input, reveal);
            group.append(row);
            if (autocomplete === 'new-password') {
                const hint = document.createElement('p');
                hint.id = `${input.id}-hint`;
                hint.className = 'auth-hint';
                hint.textContent = `At least ${minimum} characters.`;
                input.setAttribute('aria-describedby', hint.id);
                group.append(hint);
            }
        } else { group.append(input); }
        form.append(group);
        return input;
    }
    const titles = {login:'Sign in', register:'Create account', verify:'Check your email', 'request-reset':'Reset your password', reset:'Set a new password'};
    const actions = {login:'Sign in', register:'Create account', verify:'Verify email', 'request-reset':'Email me a reset token', reset:'Set new password'};
    function render(next, email = '', message = '') {
        flow = next;
        heading.textContent = titles[flow];
        form = document.createElement('form');
        form.id = `bokheim-${flow}-form`;
        form.method = 'post';
        form.action = location.pathname;
        form.autocomplete = 'on';
        error = document.createElement('p');
        error.setAttribute('role', 'alert');
        status = document.createElement('p');
        status.setAttribute('role', 'status');
        status.textContent = message;
        form.append(error, status);
        field('username', 'Email', 'email', 'username', email);
        if (flow === 'login' || flow === 'register' || flow === 'reset') {
            field('password', flow === 'reset' ? 'New password' : 'Password', 'password', flow === 'login' ? 'current-password' : 'new-password');
        }
        if (flow === 'verify') {
            const pin = field('pin', 'Verification PIN', 'text', 'one-time-code');
            pin.inputMode = 'numeric';
            pin.pattern = '[0-9]{6}';
            pin.maxLength = 6;
        }
        if (flow === 'reset') {
            field('token', 'Reset token from your email', 'text', 'off');
        }
        const primary = document.createElement('button');
        primary.type = 'submit';
        primary.textContent = actions[flow];
        const actionRow = document.createElement('div');
        actionRow.append(primary);
        if (flow === 'login') {
            actionRow.className = 'auth-actions';
            actionRow.append(button('Create account', () => {
                if (!busy) render('register', form.elements.namedItem('username').value.trim());
            }));
        }
        form.append(flow === 'login' ? actionRow : primary);
        form.addEventListener('submit', event => { event.preventDefault(); void perform(flow); });
        links = document.createElement('div');
        links.className = 'auth-links';
        const change = (label, next) => links.append(button(label, () => {
            if (!busy) render(next, form.elements.namedItem('username').value.trim());
        }));
        if (flow === 'login') {
            change('Forgot your password?', 'request-reset');
        } else if (flow === 'verify') {
            links.append(button('Resend PIN', () => void perform('resend')));
            change('Use a different email', 'register');
            change('Back to sign in', 'login');
        } else { change('Back to sign in', 'login'); }
        content.replaceChildren(form, links);
        if (dialog.open) form.elements.namedItem('username').focus();
    }
    async function perform(action) {
        if (busy || closed) return;
        const emailField = form.elements.namedItem('username');
        if (action === 'resend' ? !emailField.reportValidity() : !form.reportValidity()) return;
        // Read DOM values on submit: autofill need not dispatch typing events.
        const email = emailField.value.trim();
        const secret = (form.elements.namedItem(action === 'verify' ? 'pin' : 'password')?.value ?? '');
        const token = form.elements.namedItem('token')?.value.trim() ?? '';
        if ((action === 'register' || action === 'reset') && Array.from(secret).length < minimum) {
            error.textContent = `Use a password with at least ${minimum} characters.`;
            return;
        }
        busy = true;
        error.textContent = '';
        status.textContent = 'Please wait…';
        form.setAttribute('aria-busy', 'true');
        // Keep credential inputs discoverable until the server confirms success.
        for (const input of form.querySelectorAll('input')) input.readOnly = true;
        for (const item of dialog.querySelectorAll('button')) item.disabled = true;
        try {
            await submit(action, email, secret, token);
            if (closed) return;
            if (action === 'login' || action === 'verify') { destroy(); return; }
            if (action === 'register') render('verify', email, 'Check your email for a six-digit verification PIN.');
            if (action === 'resend') status.textContent = 'Verification PIN sent if the address exists.';
            if (action === 'request-reset') render('reset', email, 'Check your email for a reset token.');
            if (action === 'reset') render('login', email, 'Password updated. Sign in with your new password.');
        } catch (failure) {
            if (!closed) { status.textContent = ''; error.textContent = String(failure); }
        } finally {
            busy = false;
            if (!closed) {
                form.removeAttribute('aria-busy');
                for (const input of form.querySelectorAll('input')) input.readOnly = false;
                for (const item of dialog.querySelectorAll('button')) item.disabled = false;
            }
        }
    }
    dialog.addEventListener('cancel', event => { event.preventDefault(); if (!busy) destroy(); });
    render('login');
    // Opening can originate inside a GPUI click callback. Let that callback
    // release its Rust borrows before DOM focus dispatches blur/focus events.
    globalThis.addEventListener('pagehide', destroy);
    globalThis.addEventListener('popstate', destroy);
    queueMicrotask(() => {
        if (closed) return;
        document.body.append(dialog);
        dialog.showModal();
        form.elements.namedItem('username').focus();
    });
    return panel;
}
export function closeAuthentication(panel) { queueMicrotask(() => panel.destroy()); }
