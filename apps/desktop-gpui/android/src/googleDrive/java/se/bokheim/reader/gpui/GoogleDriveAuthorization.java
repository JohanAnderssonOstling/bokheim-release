package se.bokheim.reader.gpui;

import android.accounts.Account;
import android.app.Activity;
import android.app.PendingIntent;
import android.content.Intent;
import com.google.android.gms.auth.api.identity.AuthorizationClient;
import com.google.android.gms.auth.api.identity.AuthorizationRequest;
import com.google.android.gms.auth.api.identity.AuthorizationResult;
import com.google.android.gms.auth.api.identity.ClearTokenRequest;
import com.google.android.gms.auth.api.identity.Identity;
import com.google.android.gms.common.api.ApiException;
import com.google.android.gms.common.api.CommonStatusCodes;
import com.google.android.gms.common.api.Scope;
import java.util.Arrays;
import java.util.function.Consumer;

/** Owned by the Activity. Only a user-requested connection may open consent UI. */
final class GoogleDriveAuthorization {
    static final String DRIVE_SCOPE = "https://www.googleapis.com/auth/drive";
    static final int CANCELLED = 1, RECONNECT = 2, DENIED = 3, UNAVAILABLE = 4,
            OFFLINE = 5, FAILED = 6, BUSY = 7;
    interface Completion { void complete(long id, String token, int error); }
    interface Backend {
        void authorize(AuthorizationRequest request, Consumer<AuthorizationResult> success, Consumer<Exception> failure);
        void clear(String token, Runnable success, Consumer<Exception> failure);
        AuthorizationResult result(Intent data) throws ApiException;
    }
    private static final class Request {
        final long id;
        final boolean interactive;
        Request(long id, boolean interactive) { this.id = id; this.interactive = interactive; }
    }
    private final Backend backend;
    private final Consumer<PendingIntent> launch;
    private final Completion completion;
    private Request pending;
    private boolean destroyed;

    GoogleDriveAuthorization(android.content.Context context, Consumer<PendingIntent> launch, Completion completion) {
        this(new Backend() {
            final AuthorizationClient client = Identity.getAuthorizationClient(context);
            public void authorize(AuthorizationRequest request, Consumer<AuthorizationResult> success, Consumer<Exception> failure) {
                client.authorize(request).addOnSuccessListener(success::accept).addOnFailureListener(failure::accept);
            }
            public void clear(String token, Runnable success, Consumer<Exception> failure) {
                client.clearToken(ClearTokenRequest.builder().setToken(token).build()).addOnSuccessListener(unused -> success.run()).addOnFailureListener(failure::accept);
            }
            public AuthorizationResult result(Intent data) throws ApiException { return client.getAuthorizationResultFromIntent(data); }
        }, launch, completion);
    }
    GoogleDriveAuthorization(Backend backend, Consumer<PendingIntent> launch, Completion completion) {
        this.backend = backend; this.launch = launch; this.completion = completion;
    }
    void authorize(long id, String email, boolean interactive, String rejectedToken, boolean available) {
        if (!available || destroyed) { completion.complete(id, null, UNAVAILABLE); return; }
        if (pending != null) { completion.complete(id, null, BUSY); return; }
        if (!interactive && (email == null || email.isEmpty())) { completion.complete(id, null, RECONNECT); return; }
        Request request = new Request(id, interactive);
        pending = request;
        try {
            AuthorizationRequest.Builder builder = AuthorizationRequest.builder()
                    .setRequestedScopes(Arrays.asList(new Scope("openid"), new Scope("email"), new Scope(DRIVE_SCOPE)))
                    .setOptOutIncludingGrantedScopes(true);
            if (email != null && !email.isEmpty()) builder.setAccount(new Account(email, "com.google"));
            else if (interactive) builder.setPrompt(AuthorizationRequest.Prompt.SELECT_ACCOUNT);
            Runnable authorize = () -> {
                if (pending != request) return;
                try { backend.authorize(builder.build(), result -> accept(request, result), error -> fail(request, error)); }
                catch (RuntimeException error) { fail(request, error); }
            };
            if (rejectedToken != null && !rejectedToken.isEmpty()) backend.clear(rejectedToken, authorize, error -> fail(request, error));
            else authorize.run();
        } catch (RuntimeException | LinkageError error) { finish(request, null, FAILED); }
    }
    private void accept(Request request, AuthorizationResult result) {
        if (pending != request) return;
        if (result.hasResolution()) {
            if (!request.interactive) { finish(request, null, RECONNECT); return; }
            PendingIntent intent = result.getPendingIntent();
            if (intent == null) { finish(request, null, FAILED); return; }
            try { launch.accept(intent); } catch (RuntimeException error) { finish(request, null, FAILED); }
        } else if (!result.getGrantedScopes().contains(DRIVE_SCOPE)) finish(request, null, DENIED);
        else {
            String token = result.getAccessToken();
            finish(request, token, token == null || token.isEmpty() ? FAILED : 0);
        }
    }
    void result(int resultCode, Intent data) {
        Request request = pending;
        if (request == null) return;
        if (resultCode != Activity.RESULT_OK || data == null) { finish(request, null, CANCELLED); return; }
        try { accept(request, backend.result(data)); } catch (ApiException | RuntimeException error) { fail(request, error); }
    }
    private void fail(Request request, Exception error) {
        int status = error instanceof ApiException ? ((ApiException) error).getStatusCode() : CommonStatusCodes.ERROR;
        int failure = status == CommonStatusCodes.NETWORK_ERROR ? OFFLINE
                : status == CommonStatusCodes.SIGN_IN_REQUIRED ? RECONNECT
                : status == CommonStatusCodes.CANCELED ? CANCELLED : FAILED;
        // SDK exception messages and result payloads can contain account data.
        finish(request, null, failure);
    }
    private void finish(Request request, String token, int error) {
        if (pending != request) return;
        pending = null;
        completion.complete(request.id, token, error);
    }
    void destroy() {
        destroyed = true;
        if (pending != null) finish(pending, null, CANCELLED);
    }
}
