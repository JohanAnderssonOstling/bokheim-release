package se.bokheim.reader.gpui;

import static org.junit.Assert.*;
import android.app.Activity;
import android.view.View;
import android.view.ViewGroup;
import android.view.autofill.AutofillManager;
import android.widget.Button;
import android.widget.EditText;
import org.json.JSONObject;
import org.junit.After;
import org.junit.Before;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.Robolectric;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.android.controller.ActivityController;
import org.robolectric.annotation.Config;
import org.robolectric.annotation.Implementation;
import org.robolectric.annotation.Implements;
import java.util.ArrayList;
import java.util.List;

@RunWith(RobolectricTestRunner.class)
@Config(sdk = 30, shadows = AccountDialogTest.Autofill.class)
public class AccountDialogTest {
    @Implements(AutofillManager.class)
    public static class Autofill {
        static int commits, cancels;
        @Implementation protected void commit() { commits++; }
        @Implementation protected void cancel() { cancels++; }
    }
    private ActivityController<Activity> controller;
    private AccountDialog dialog;
    private final List<JSONObject> requests = new ArrayList<>();
    @Before public void setup() {
        Autofill.commits = Autofill.cancels = 0;
        controller = Robolectric.buildActivity(Activity.class).setup();
        dialog = new AccountDialog(controller.get(), 42, 12, new int[]{-1,0xff222222,0xff555555,0xff224488,-1}, (id, message) -> {
            assertEquals(42, id);
            try { requests.add(new JSONObject(message)); } catch (Exception error) { throw new AssertionError(error); }
        });
        dialog.show();
    }
    @After public void teardown() { dialog.dismiss(); controller.pause().stop().destroy(); }
    private EditText field(int id) { return dialog.findViewById(id); }
    private Button find(View view, String label) {
        if (view instanceof Button && ((Button)view).getText().toString().equals(label)) return (Button)view;
        if (view instanceof ViewGroup) for (int i=0;i<((ViewGroup)view).getChildCount();i++) {
            Button result = find(((ViewGroup)view).getChildAt(i),label); if (result != null) return result;
        }
        return null;
    }
    private void click(String label) { Button b = find(dialog.getWindow().getDecorView(),label); assertNotNull(label,b); b.performClick(); }
    private void credentials(String password) { field(R.id.account_email).setText("reader@example.invalid"); field(R.id.account_password).setText(password); }
    @Test public void autofilledValuesSubmitAndFailurePreservesFields() throws Exception {
        EditText password = field(R.id.account_password);
        assertArrayEquals(new String[]{View.AUTOFILL_HINT_USERNAME},field(R.id.account_email).getAutofillHints());
        assertArrayEquals(new String[]{View.AUTOFILL_HINT_PASSWORD},password.getAutofillHints());
        assertFalse(password.isSaveEnabled());
        assertTrue(password.getTransformationMethod() instanceof android.text.method.PasswordTransformationMethod);
        credentials("filled password");
        click("Sign in"); click("Sign in"); click("Close"); dialog.onBackPressed();
        assertEquals(1,requests.size()); assertTrue(dialog.isShowing());
        assertEquals("filled password",requests.get(0).getString("secret"));
        dialog.complete("Incorrect password");
        assertSame(password,field(R.id.account_password));
        assertEquals("filled password",password.getText().toString());
        assertEquals(0,Autofill.commits);
        click("Show password"); assertNull(password.getTransformationMethod());
        click("Hide password"); assertNotNull(password.getTransformationMethod());
        click("Sign in"); dialog.complete(null);
        assertEquals(1,Autofill.commits); assertFalse(dialog.isShowing());
    }
    @Test public void registrationVerificationAndResetUseCorrectHints() throws Exception {
        click("Create account");
        assertArrayEquals(new String[]{"newPassword"},field(R.id.account_password).getAutofillHints());
        credentials("short"); click("Create account"); assertTrue(requests.isEmpty());
        credentials("long new password"); click("Create account"); dialog.complete(null);
        assertEquals(0,Autofill.commits);
        assertNull(field(R.id.account_password));
        click("Resend PIN"); assertEquals("resend",requests.get(1).getString("action")); dialog.complete(null);
        field(R.id.account_pin).setText("123456"); click("Verify email");
        assertEquals("123456",requests.get(2).getString("secret"));
        dialog.complete("Invalid PIN");
        click("Back to sign in"); assertEquals("",field(R.id.account_password).getText().toString());
        click("Forgot password?"); click("Email me a reset token"); dialog.complete(null);
        assertArrayEquals(new String[]{"newPassword"},field(R.id.account_password).getAutofillHints());
        field(R.id.account_password).setText("replacement password"); field(R.id.account_reset_token).setText("reset-token");
        click("Set new password"); assertEquals("reset-token",requests.get(4).getString("token")); dialog.complete(null);
        assertEquals(0,Autofill.commits);
        assertEquals("",field(R.id.account_password).getText().toString());
        assertArrayEquals(new String[]{View.AUTOFILL_HINT_PASSWORD},field(R.id.account_password).getAutofillHints());
    }
    @Test public void dismissalCancelsAndIgnoresLateResponse() {
        credentials("filled password"); click("Sign in");
        dialog.dismiss(); dialog.complete(null);
        assertEquals(0,Autofill.commits); assertEquals(1,Autofill.cancels);
        assertFalse(dialog.isShowing());
    }
    @Test @Config(sdk = 26) public void worksOnMinimumAndroidVersion() {
        credentials("filled password"); click("Sign in"); dialog.complete(null);
        assertFalse(dialog.isShowing());
    }
}
