package se.bokheim.reader.gpui;

import static org.junit.Assert.*;

import android.app.Activity;
import android.graphics.Rect;
import android.text.method.PasswordTransformationMethod;
import android.view.View;
import android.view.ViewGroup;
import android.widget.Button;
import android.widget.EditText;
import android.widget.ScrollView;
import android.widget.TextView;
import androidx.core.graphics.Insets;
import androidx.core.view.ViewCompat;
import androidx.core.view.WindowInsetsCompat;
import java.util.ArrayList;
import java.util.List;
import org.json.JSONObject;
import org.json.JSONTokener;
import org.junit.After;
import org.junit.Before;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.Robolectric;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.android.controller.ActivityController;
import org.robolectric.annotation.Config;

@RunWith(RobolectricTestRunner.class)
@Config(sdk = 30)
public class AccountDialogTest {
    private static final int[] LIGHT = {0xfff6f2eb, 0xff22201d, 0xff6b675f, 0xff466853, 0xfffffdfa,
        0xff8c7f64, 0xff8a6a2b, 0xffa13029, 0xffd9e8de};
    private static final int[] DARK = {0xff11100d, 0xffefe6d0, 0xffa69c86, 0xffd3b271, 0xff11100d,
        0xff786644, 0xffd3b271, 0xffdf8a83, 0xff3c3423};
    private ActivityController<Activity> controller;
    private Activity activity;
    private AccountDialog dialog;
    private final List<JSONObject> requests = new ArrayList<>();
    private int closedEvents;

    @Before public void setUp() {
        controller = Robolectric.buildActivity(Activity.class).setup();
        activity = controller.get();
    }

    @After public void tearDown() {
        if (dialog != null) {
            dialog.dismiss();
            assertEquals(1, closedEvents);
        }
        controller.pause().stop().destroy();
    }

    private void open(int[] colors) {
        dialog = new AccountDialog(activity, 42, 12, colors, (id, message) -> {
            assertEquals(42, id);
            try {
                Object event = new JSONTokener(message).nextValue();
                if (event instanceof String) {
                    assertEquals("closed", event);
                    closedEvents++;
                } else {
                    JSONObject envelope = (JSONObject) event;
                    assertEquals(1, envelope.length());
                    requests.add(envelope.getJSONObject("submit"));
                }
            }
            catch (Exception error) { throw new AssertionError(error); }
        });
        dialog.show();
    }

    private int dp(int value) { return Math.round(value * activity.getResources().getDisplayMetrics().density); }

    private ViewGroup content() { return dialog.findViewById(android.R.id.content); }

    private void layout(int width, int height) {
        content().measure(View.MeasureSpec.makeMeasureSpec(dp(width), View.MeasureSpec.EXACTLY),
            View.MeasureSpec.makeMeasureSpec(dp(height), View.MeasureSpec.EXACTLY));
        content().layout(0, 0, dp(width), dp(height));
    }

    private TextView findText(View view, String label) {
        return findText(view, label, false);
    }

    private TextView findText(View view, String label, boolean buttonsOnly) {
        if (view instanceof TextView && (!buttonsOnly || view instanceof Button) && ((TextView) view).getText().toString().equals(label)) return (TextView) view;
        if (view instanceof ViewGroup) {
            ViewGroup group = (ViewGroup) view;
            for (int i = 0; i < group.getChildCount(); i++) {
                TextView found = findText(group.getChildAt(i), label, buttonsOnly);
                if (found != null) return found;
            }
        }
        return null;
    }

    private Button button(String label) {
        TextView found = findText(content(), label, true);
        assertTrue("Missing button: " + label, found instanceof Button);
        return (Button) found;
    }

    private EditText field(int id) { return dialog.findViewById(id); }

    @Test public void narrowScreenKeepsFieldsAndTouchTargetsInsideThePage() {
        open(LIGHT);
        layout(320, 640);
        assertTrue((dialog.getWindow().getDecorView().getSystemUiVisibility() & View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR) != 0);
        for (int id : new int[]{R.id.account_email, R.id.account_password}) {
            EditText input = field(id);
            Rect bounds = new Rect();
            input.getDrawingRect(bounds);
            content().offsetDescendantRectToMyCoords(input, bounds);
            assertTrue(bounds.left >= dp(24));
            assertTrue(bounds.right <= dp(296));
            assertTrue(bounds.height() >= dp(56));
        }
        for (String label : new String[]{"Close", "Show password", "Sign in", "Forgot password?", "Create account"}) {
            assertTrue(label, button(label).getHeight() >= dp(48));
        }
        Rect primary = new Rect(), secondary = new Rect();
        button("Sign in").getGlobalVisibleRect(primary);
        button("Forgot password?").getGlobalVisibleRect(secondary);
        assertTrue(secondary.top >= primary.bottom + dp(8));
        assertNull(findText(content(), "Please wait…"));
    }

    @Test public void wideScreenCentersABoundedForm() {
        open(LIGHT);
        layout(1000, 700);
        EditText email = field(R.id.account_email);
        Rect bounds = new Rect();
        email.getDrawingRect(bounds);
        content().offsetDescendantRectToMyCoords(email, bounds);
        assertEquals(dp(432), bounds.width());
        assertEquals(dp(500), bounds.centerX(), 1);
    }

    @Test public void keyboardAndSystemInsetsLeaveAReachableScrollableForm() {
        open(DARK);
        assertEquals(0, dialog.getWindow().getDecorView().getSystemUiVisibility() & View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR);
        ScrollView scroll = (ScrollView) content().getChildAt(0);
        WindowInsetsCompat insets = new WindowInsetsCompat.Builder()
            .setInsets(WindowInsetsCompat.Type.systemBars(), Insets.of(dp(8), dp(24), dp(8), dp(24)))
            .setInsets(WindowInsetsCompat.Type.ime(), Insets.of(0, 0, 0, dp(280)))
            .setVisible(WindowInsetsCompat.Type.ime(), true).build();
        ViewCompat.dispatchApplyWindowInsets(scroll, insets);
        layout(320, 480);
        assertEquals(dp(24), scroll.getPaddingTop());
        assertEquals(dp(280), scroll.getPaddingBottom());
        assertTrue(scroll.getChildAt(0).getHeight() > scroll.getHeight() - scroll.getPaddingTop() - scroll.getPaddingBottom());
        assertEquals(dp(8), scroll.getPaddingLeft());
        WindowInsetsCompat hidden = new WindowInsetsCompat.Builder(insets)
            .setInsets(WindowInsetsCompat.Type.ime(), Insets.NONE).setVisible(WindowInsetsCompat.Type.ime(), false).build();
        ViewCompat.dispatchApplyWindowInsets(scroll, hidden);
        assertEquals(dp(24), scroll.getPaddingBottom());
    }

    @Test public void darkThemePreservesCredentialsAndShowsReadableRequestErrors() throws Exception {
        open(DARK);
        field(R.id.account_email).setText("reader@example.com");
        field(R.id.account_password).setText("correct horse battery");
        assertEquals(DARK[1], field(R.id.account_email).getCurrentTextColor());
        assertEquals(DARK[4], button("Sign in").getCurrentTextColor());
        assertEquals(DARK[6], button("Create account").getCurrentTextColor());
        button("Show password").performClick();
        assertNull(field(R.id.account_password).getTransformationMethod());
        button("Hide password").performClick();
        assertTrue(field(R.id.account_password).getTransformationMethod() instanceof PasswordTransformationMethod);
        button("Sign in").performClick();
        assertFalse(button("Sign in").isEnabled());
        assertEquals("login", requests.get(0).getString("action"));
        assertEquals("correct horse battery", requests.get(0).getString("password"));
        dialog.complete("Could not sign in");
        TextView feedback = findText(content(), "Could not sign in");
        assertEquals(View.VISIBLE, feedback.getVisibility());
        assertEquals(DARK[7], feedback.getCurrentTextColor());
        assertEquals("reader@example.com", field(R.id.account_email).getText().toString());
        assertEquals("correct horse battery", field(R.id.account_password).getText().toString());
        assertTrue(button("Sign in").isEnabled());
    }

    @Test public void registrationAndRecoveryKeepNativeFieldIdentitiesAndSubmitTheirCodes() throws Exception {
        open(LIGHT);
        field(R.id.account_email).setText("reader@example.com");
        button("Create account").performClick();
        assertEquals("newUsername", field(R.id.account_email).getAutofillHints()[0]);
        assertEquals("newPassword", field(R.id.account_password).getAutofillHints()[0]);
        field(R.id.account_password).setText("correct horse battery");
        button("Create account").performClick();
        dialog.complete(null);
        assertNotNull(field(R.id.account_pin));
        assertNotNull(field(R.id.account_password));
        assertEquals("", field(R.id.account_password).getText().toString());
        field(R.id.account_password).setText("correct horse battery");
        field(R.id.account_pin).setText("123456");
        button("Verify email").performClick();
        JSONObject verification = requests.get(requests.size() - 1);
        assertEquals("verify", verification.getString("action"));
        assertEquals("reader@example.com", verification.getString("email"));
        assertEquals("123456", verification.getString("pin"));
        assertEquals("correct horse battery", verification.getString("password"));
        dialog.complete("Check the PIN and try again");
        button("Resend PIN").performClick();
        dialog.complete(null);
        assertEquals("123456", field(R.id.account_pin).getText().toString());
        assertEquals("correct horse battery", field(R.id.account_password).getText().toString());
        button("Back to sign in").performClick();
        button("Forgot password?").performClick();
        button("Email me a reset code").performClick();
        dialog.complete(null);
        field(R.id.account_password).setText("another long password");
        field(R.id.account_reset_token).setText("reset-code");
        button("Set new password").performClick();
        JSONObject reset = requests.get(requests.size() - 1);
        assertEquals("reset", reset.getString("action"));
        assertEquals("reset-code", reset.getString("token"));
        assertEquals("another long password", reset.getString("password"));
        assertFalse(reset.has("email"));
        dialog.complete(null);
        assertEquals("", field(R.id.account_password).getText().toString());
        assertEquals(LIGHT[2], findText(content(), "Password updated. Sign in with your new password.").getCurrentTextColor());
    }
}
