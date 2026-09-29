package se.bokheim.reader.gpui;

import android.app.Activity;
import android.app.Dialog;
import android.graphics.Typeface;
import android.content.res.ColorStateList;
import android.text.InputType;
import android.text.method.PasswordTransformationMethod;
import android.view.View;
import android.view.WindowManager;
import android.view.autofill.AutofillManager;
import android.view.inputmethod.EditorInfo;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;
import org.json.JSONObject;
import org.json.JSONException;
import java.util.ArrayList;
import java.util.List;

/** Real Android fields, with one stable form throughout an account request. */
final class AccountDialog extends Dialog {
    interface Submit { void send(long id, String message); }
    final long sessionId;
    private final Submit submit;
    private final int minimum;
    private final int[] colors;
    private final LinearLayout form;
    private final AutofillManager autofill;
    private final List<Button> buttons = new ArrayList<>();
    private String flow = "login";
    private String pending;
    private boolean busy, finished, committed;
    private EditText email, password, pin, token;
    private TextView feedback;

    AccountDialog(Activity activity, long id, int minimum, int[] colors, Submit submit) {
        super(activity, android.R.style.Theme_Material_Light_NoActionBar);
        this.sessionId = id;
        this.minimum = minimum;
        this.colors = colors;
        this.submit = submit;
        autofill = activity.getSystemService(AutofillManager.class);
        ScrollView scroll = new ScrollView(getContext());
        scroll.setFillViewport(true);
        scroll.setFitsSystemWindows(true);
        scroll.setBackgroundColor(colors[0]);
        form = new LinearLayout(getContext());
        form.setOrientation(LinearLayout.VERTICAL);
        form.setPadding(dp(24), dp(24), dp(24), dp(24));
        scroll.addView(form);
        setContentView(scroll);
        getWindow().setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE);
        setCanceledOnTouchOutside(false);
        render("login", "", "");
    }
    @Override public void dismiss() {
        // Dialog's dismiss listener is asynchronous. Mark this session closed
        // now, before an already queued network response can commit credentials.
        if (!finished) {
            finished = true;
            if (!committed && autofill != null) autofill.cancel();
            emit("closed", "", "", "");
        }
        super.dismiss();
    }
    @Override public void show() {
        super.show();
        getWindow().setLayout(WindowManager.LayoutParams.MATCH_PARENT, WindowManager.LayoutParams.MATCH_PARENT);
        email.requestFocus();
    }
    private int dp(int value) { return Math.round(value * getContext().getResources().getDisplayMetrics().density); }
    private TextView text(String value, int size) {
        TextView view = new TextView(getContext());
        view.setText(value);
        view.setTextSize(size);
        view.setTextColor(colors[1]);
        view.setTypeface(Typeface.SERIF);
        view.setPadding(0, dp(8), 0, dp(8));
        form.addView(view);
        return view;
    }
    private EditText field(int id, String label, int inputType, String hint, String value) {
        TextView caption = text(label, 16);
        caption.setLabelFor(id);
        EditText input = new EditText(getContext());
        input.setId(id);
        input.setInputType(inputType);
        input.setSingleLine(true);
        input.setTextColor(colors[1]);
        input.setBackgroundTintList(ColorStateList.valueOf(colors[3]));
        input.setTypeface(Typeface.SERIF);
        input.setSaveEnabled(false);
        input.setImportantForAutofill(hint == null ? View.IMPORTANT_FOR_AUTOFILL_NO : View.IMPORTANT_FOR_AUTOFILL_YES);
        if (hint != null) input.setAutofillHints(hint);
        input.setText(value);
        input.setImeOptions(EditorInfo.IME_ACTION_NEXT | EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING);
        form.addView(input, new LinearLayout.LayoutParams(-1, dp(56)));
        return input;
    }
    private Button button(String label, Runnable action) {
        Button button = new Button(getContext());
        button.setText(label);
        button.setAllCaps(false);
        button.setTextColor(colors[1]);
        button.setTypeface(Typeface.SERIF);
        button.setOnClickListener(ignored -> { if (!busy) action.run(); });
        buttons.add(button);
        form.addView(button, new LinearLayout.LayoutParams(-1, -2));
        return button;
    }
    private void change(String next) {
        if (autofill != null) autofill.cancel();
        render(next, email.getText().toString().trim(), "");
    }
    private void render(String next, String address, String message) {
        flow = next;
        committed = false;
        form.removeAllViews();
        buttons.clear();
        password = pin = token = null;
        String title;
        switch (flow) {
            case "register": title = "Create account"; break;
            case "verify": title = "Check your email"; break;
            case "request-reset": title = "Reset your password"; break;
            case "reset": title = "Set a new password"; break;
            default: title = "Sign in";
        }
        setTitle(title);
        TextView heading = text(title, 28);
        if (android.os.Build.VERSION.SDK_INT >= 28) heading.setAccessibilityHeading(true);
        TextView identity = text("Bokheim · app.bokheim.se", 16);
        identity.setTextColor(colors[2]);
        feedback = text(message, 16);
        feedback.setAccessibilityLiveRegion(View.ACCESSIBILITY_LIVE_REGION_POLITE);
        email = field(R.id.account_email, "Email", InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS,
            flow.equals("register") ? "newUsername" : View.AUTOFILL_HINT_USERNAME, address);
        if (flow.equals("login") || flow.equals("register") || flow.equals("reset")) {
            password = field(R.id.account_password, flow.equals("reset") ? "New password" : "Password",
                InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD,
                flow.equals("login") ? View.AUTOFILL_HINT_PASSWORD : "newPassword", "");
            password.setTransformationMethod(PasswordTransformationMethod.getInstance());
            Button reveal = button("Show password", () -> {});
            reveal.setOnClickListener(ignored -> {
                if (busy) return;
                boolean hidden = password.getTransformationMethod() instanceof PasswordTransformationMethod;
                password.setTransformationMethod(hidden ? null : PasswordTransformationMethod.getInstance());
                password.setSelection(password.length());
                reveal.setText(hidden ? "Hide password" : "Show password");
            });
            if (!flow.equals("login")) text("Use at least " + minimum + " characters.", 14);
        }
        if (flow.equals("verify")) pin = field(R.id.account_pin, "Verification PIN", InputType.TYPE_CLASS_NUMBER, null, "");
        if (flow.equals("reset")) token = field(R.id.account_reset_token, "Reset token from your email", InputType.TYPE_CLASS_TEXT, null, "");
        EditText last = token != null ? token : pin != null ? pin : password != null ? password : email;
        last.setImeOptions(EditorInfo.IME_ACTION_DONE | EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING);
        last.setOnEditorActionListener((v, action, event) -> {
            if (action == EditorInfo.IME_ACTION_DONE) { perform(flow); return true; }
            return false;
        });
        String action = flow.equals("verify") ? "Verify email" : flow.equals("request-reset") ? "Email me a reset token" : flow.equals("reset") ? "Set new password" : title;
        Button primary = button(action, () -> perform(flow));
        primary.setBackgroundTintList(ColorStateList.valueOf(colors[3]));
        primary.setTextColor(colors[4]);
        if (flow.equals("login")) {
            button("Forgot password?", () -> change("request-reset"));
            button("Create account", () -> change("register"));
        } else {
            if (flow.equals("verify")) button("Resend PIN", () -> perform("resend"));
            button("Back to sign in", () -> change("login"));
        }
        button("Close", this::dismiss);
        email.requestFocus();
    }
    private void setBusy(boolean value) {
        busy = value;
        setCancelable(!value);
        for (Button button : buttons) button.setEnabled(!value);
        for (EditText field : new EditText[]{email, password, pin, token}) if (field != null) field.setEnabled(!value);
    }
    private void perform(String action) {
        if (busy || finished) return;
        String address = email.getText().toString().trim();
        String secret = pin != null && action.equals("verify") ? pin.getText().toString() : password != null ? password.getText().toString() : "";
        String resetToken = token == null ? "" : token.getText().toString().trim();
        if (!android.util.Patterns.EMAIL_ADDRESS.matcher(address).matches()) { email.setError("Enter a valid email address"); email.requestFocus(); return; }
        if ((action.equals("login") && secret.isEmpty()) || ((action.equals("register") || action.equals("reset")) && secret.codePointCount(0, secret.length()) < minimum)) {
            password.setError(action.equals("login") ? "Enter your password" : "Use at least " + minimum + " characters"); password.requestFocus(); return;
        }
        if (action.equals("verify") && !secret.matches("[0-9]{6}")) { pin.setError("Enter the six-digit PIN"); pin.requestFocus(); return; }
        if (action.equals("reset") && resetToken.isEmpty()) { token.setError("Enter the reset token"); token.requestFocus(); return; }
        pending = action;
        setBusy(true);
        feedback.setText("Please wait…");
        emit(action, address, secret, resetToken);
    }
    private void emit(String action, String email, String secret, String token) {
        try {
            JSONObject message = new JSONObject().put("action", action).put("email", email).put("secret", secret).put("token", token);
            submit.send(sessionId, message.toString());
        } catch (JSONException impossible) { throw new IllegalStateException(impossible); }
    }
    void complete(String error) {
        if (!busy || finished) return;
        setBusy(false);
        if (error != null) { feedback.setText(error); return; }
        String address = email.getText().toString().trim();
        if (pending.equals("login")) {
            // Only sign-in proves this exact email/password pair is valid.
            // Registration hides conflicts; reset tokens may belong to another
            // address. Never offer those unverified pairs as saved credentials.
            if (autofill != null) autofill.commit();
            committed = true;
        } else if (!pending.equals("resend") && autofill != null) {
            autofill.cancel();
        }
        switch (pending) {
            case "login": case "verify": dismiss(); break;
            case "register": render("verify", address, "Check your email for a six-digit verification PIN."); break;
            case "resend": feedback.setText("Verification PIN sent if the address exists."); break;
            case "request-reset": render("reset", address, "Check your email for a reset token."); break;
            case "reset": render("login", address, "Password updated. Sign in with your new password."); break;
        }
    }
}
