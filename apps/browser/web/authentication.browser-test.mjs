// Exercises the real DOM in browsers with a fake account API. This establishes
// form semantics and lifecycle, not actual password-manager extension support.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
const {chromium, firefox} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const module = await readFile(new URL('./authentication.js', import.meta.url));
const server = createServer((request, response) => {
    response.setHeader('Content-Type', request.url === '/authentication.js' ? 'text/javascript' : 'text/html');
    response.end(request.url === '/authentication.js' ? module : '<button id="opener">Sign in</button>');
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
try {
    for (const [engine, executablePath] of [[chromium, process.env.CHROMIUM_PATH], [firefox, process.env.FIREFOX_PATH]]) {
        const browser = await engine.launch({headless:true, executablePath});
        try {
            const page = await browser.newPage({viewport:{width:390,height:740}});
            const errors = [];
            page.on('pageerror', e => errors.push(e.message));
            await page.goto(`http://127.0.0.1:${server.address().port}/`);
            await page.evaluate(async () => {
                const {openAuthentication} = await import('/authentication.js');
                window.calls = [];
                window.behavior = 'reject';
                window.openPanel = () => {
                    document.querySelector('#opener').focus();
                    window.panel = openAuthentication(JSON.stringify({background:'#f5f1e7',text:'#243c30',muted:'#666',border:'#999',accent:'#243c30',accentText:'#fff'}),12,
                        async (...args) => {
                            calls.push(args);
                            if (behavior === 'reject') throw Error('Incorrect email or password');
                            if (behavior === 'pending') await new Promise(resolve => window.finish = resolve);
                        });
                };
                document.querySelector('#opener').onclick = openPanel;
                openPanel();
            });
            const email = page.getByLabel('Email', {exact:true});
            const password = page.getByLabel('Password', {exact:true});
            const signIn = page.locator('dialog').getByRole('button', {name:'Sign in',exact:true});
            assert.equal(await email.getAttribute('autocomplete'), 'username');
            assert.equal(await password.getAttribute('autocomplete'), 'current-password');
            assert.equal(await password.getAttribute('type'), 'password');
            // Simulate autofill that changes values without emitting input/change.
            await page.evaluate(() => {
                document.querySelector('[name=username]').value = 'reader@example.com';
                document.querySelector('[name=password]').value = 'saved-password';
                window.originalForm = document.querySelector('form');
            });
            await signIn.click();
            await page.getByRole('alert').filter({hasText:'Incorrect email or password'}).waitFor();
            assert.equal(await page.evaluate(() => originalForm === document.querySelector('form')), true);
            assert.deepEqual(await page.evaluate(() => calls[0]), ['login','reader@example.com','saved-password','']);
            await page.getByRole('button',{name:'Show password'}).click();
            assert.equal(await password.getAttribute('type'), 'text');
            await page.getByRole('button',{name:'Hide password'}).click();
            await page.evaluate(() => behavior = 'pending');
            await password.press('Enter');
            await page.waitForFunction(() => typeof finish === 'function');
            assert.equal(await page.getByRole('button',{name:'Close sign in'}).isDisabled(), true);
            await page.keyboard.press('Escape');
            assert.equal(await page.locator('dialog').count(), 1);
            await page.evaluate(() => document.querySelector('form').dispatchEvent(new Event('submit',{cancelable:true})));
            assert.equal(await page.evaluate(() => calls.length), 2);
            await page.evaluate(() => finish());
            await page.locator('dialog').waitFor({state:'detached'});
            assert.equal(await page.locator('input[type=password]').count(), 0);
            assert.equal(await page.evaluate(() => document.activeElement.id), 'opener');
            await page.evaluate(() => { behavior = 'success'; openPanel(); });
            await page.getByRole('button',{name:'Create account',exact:true}).click();
            assert.equal(await password.getAttribute('autocomplete'), 'new-password');
            await email.fill('new@example.com');
            await password.fill('short');
            await page.getByRole('button',{name:'Create account',exact:true}).click();
            await page.getByRole('alert').filter({hasText:'at least 12'}).waitFor();
            assert.equal(await page.evaluate(() => calls.length), 2);
            await password.fill('generated-password');
            await page.getByRole('button',{name:'Create account',exact:true}).click();
            await page.getByRole('heading',{name:'Check your email'}).waitFor();
            assert.equal(await page.locator('[name=password]').count(), 0);
            assert.equal(await email.inputValue(), 'new@example.com');
            assert.equal(await page.getByLabel('Verification PIN').getAttribute('autocomplete'), 'one-time-code');
            await page.getByRole('button',{name:'Resend PIN'}).click();
            await page.getByRole('status').filter({hasText:'Verification PIN sent'}).waitFor();
            await page.getByLabel('Verification PIN').fill('123456');
            await page.getByRole('button',{name:'Verify email'}).click();
            await page.locator('dialog').waitFor({state:'detached'});
            await page.evaluate(() => openPanel());
            await email.fill('reader@example.com');
            await page.getByRole('button',{name:'Forgot your password?'}).click();
            await page.getByRole('button',{name:'Email me a reset token'}).click();
            await page.getByRole('heading',{name:'Set a new password'}).waitFor();
            assert.equal(await page.getByLabel('New password',{exact:true}).getAttribute('autocomplete'), 'new-password');
            await page.getByLabel('New password',{exact:true}).fill('replacement-password');
            await page.getByLabel('Reset token from your email').fill('reset-token');
            await page.getByRole('button',{name:'Set new password'}).click();
            await page.getByRole('status').filter({hasText:'Password updated'}).waitFor();
            assert.equal(await password.inputValue(), '');
            assert.deepEqual(await page.evaluate(() => calls.slice(2)), [
                ['register','new@example.com','generated-password',''],
                ['resend','new@example.com','',''],
                ['verify','new@example.com','123456',''],
                ['request-reset','reader@example.com','',''],
                ['reset','reader@example.com','replacement-password','reset-token'],
            ]);
            const bounds = await page.locator('dialog').boundingBox();
            assert.ok(bounds.x >= 0 && bounds.x + bounds.width <= 390);
            if (process.env.AUTH_SCREENSHOT && engine === chromium) await page.screenshot({path:process.env.AUTH_SCREENSHOT});
            await page.keyboard.press('Escape');
            await page.locator('dialog').waitFor({state:'detached'});
            await page.evaluate(() => openPanel());
            await page.evaluate(() => dispatchEvent(new PopStateEvent('popstate')));
            await page.locator('dialog').waitFor({state:'detached'});
            assert.deepEqual(errors, []);
            console.log(`PASS ${engine.name()}: autofill values, semantic fields, errors, pending requests, registration, verification, reset, focus, and mobile bounds`);
        } finally { await browser.close(); }
    }
} finally { server.close(); }
