package se.bokheim.reader.gpui;

import static org.junit.Assert.*;
import android.app.Activity;
import android.app.PendingIntent;
import android.content.Intent;
import com.google.android.gms.auth.api.identity.AuthorizationRequest;
import com.google.android.gms.auth.api.identity.AuthorizationResult;
import com.google.android.gms.common.api.ApiException;
import com.google.android.gms.common.api.CommonStatusCodes;
import com.google.android.gms.common.api.Status;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.function.Consumer;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.RuntimeEnvironment;
import org.robolectric.annotation.Config;

@RunWith(RobolectricTestRunner.class)
@Config(sdk = 30)
public class GoogleDriveAuthorizationTest {
    static class Backend implements GoogleDriveAuthorization.Backend {
        AuthorizationRequest request;
        Consumer<AuthorizationResult> success;
        Consumer<Exception> failure;
        String cleared;
        AuthorizationResult returned;
        public void authorize(AuthorizationRequest request, Consumer<AuthorizationResult> success, Consumer<Exception> failure) { this.request = request; this.success = success; this.failure = failure; }
        public void clear(String token, Runnable success, Consumer<Exception> failure) { cleared = token; success.run(); }
        public AuthorizationResult result(Intent data) { return returned; }
    }
    static class Result {
        long id; String token; int error; int count;
        void complete(long id, String token, int error) { this.id=id; this.token=token; this.error=error; count++; }
    }
    private AuthorizationResult granted(boolean drive) {
        return new AuthorizationResult(null, "fixture-token", null,
                drive ? Collections.singletonList(GoogleDriveAuthorization.DRIVE_SCOPE) : Collections.emptyList(), null, null);
    }
    private AuthorizationResult consent() {
        PendingIntent pending = PendingIntent.getActivity(RuntimeEnvironment.getApplication(), 1, new Intent("fixture-consent"), PendingIntent.FLAG_IMMUTABLE);
        return new AuthorizationResult(null, null, null, Collections.emptyList(), null, pending);
    }
    @Test public void interactiveConnectionResolvesConsentBeforeDeliveringTheToken() {
        Backend backend = new Backend(); Result result = new Result(); List<PendingIntent> launched = new ArrayList<>();
        GoogleDriveAuthorization authorization = new GoogleDriveAuthorization(backend, launched::add, result::complete);
        authorization.authorize(1, "", true, "", true);
        backend.success.accept(consent());
        assertEquals(1, launched.size()); assertEquals(0, result.count);
        backend.returned = granted(true);
        authorization.result(Activity.RESULT_OK, new Intent());
        assertEquals("fixture-token", result.token); assertEquals(0, result.error); assertEquals(1, result.count);
    }
    @Test public void silentRefreshNeverOpensConsentAndIsBoundToTheExistingAccount() {
        Backend backend = new Backend(); Result result = new Result(); List<PendingIntent> launched = new ArrayList<>();
        GoogleDriveAuthorization authorization = new GoogleDriveAuthorization(backend, launched::add, result::complete);
        authorization.authorize(2, "reader@example.com", false, "rejected-token", true);
        assertEquals("rejected-token", backend.cleared);
        assertEquals("reader@example.com", backend.request.getAccount().name);
        backend.success.accept(consent());
        assertTrue(launched.isEmpty()); assertEquals(GoogleDriveAuthorization.RECONNECT, result.error);
    }
    @Test public void aPartialGrantDoesNotConnectDrive() {
        Backend backend = new Backend(); Result result = new Result();
        GoogleDriveAuthorization authorization = new GoogleDriveAuthorization(backend, ignored -> fail(), result::complete);
        authorization.authorize(3, "", true, "", true);
        backend.success.accept(granted(false));
        assertEquals(GoogleDriveAuthorization.DENIED, result.error); assertNull(result.token);
    }
    @Test public void cancelledOrDestroyedActivitiesRejectLateCallbacks() {
        Backend backend = new Backend(); Result result = new Result();
        GoogleDriveAuthorization authorization = new GoogleDriveAuthorization(backend, ignored -> { }, result::complete);
        authorization.authorize(4, "", true, "", true);
        authorization.result(Activity.RESULT_CANCELED, null);
        backend.success.accept(granted(true));
        assertEquals(1, result.count); assertEquals(GoogleDriveAuthorization.CANCELLED, result.error);
        authorization.authorize(5, "", true, "", true);
        authorization.destroy(); backend.success.accept(granted(true));
        assertEquals(2, result.count); assertNull(result.token);
    }
    @Test public void networkFailureFinishesTheRequestAndAllowsASilentRetry() {
        Backend backend = new Backend(); Result result = new Result();
        GoogleDriveAuthorization authorization = new GoogleDriveAuthorization(backend, ignored -> fail(), result::complete);
        authorization.authorize(7, "reader@example.com", false, "", true);
        backend.failure.accept(new ApiException(new Status(CommonStatusCodes.NETWORK_ERROR)));
        assertEquals(GoogleDriveAuthorization.OFFLINE, result.error); assertNull(result.token);
        authorization.authorize(8, "reader@example.com", false, "", true);
        backend.success.accept(granted(true));
        assertEquals(8, result.id); assertEquals(0, result.error); assertEquals(2, result.count);
    }
    @Test public void aConcurrentRequestCannotReplaceAnExistingConsentAttempt() {
        Backend backend = new Backend(); Result result = new Result();
        GoogleDriveAuthorization authorization = new GoogleDriveAuthorization(backend, ignored -> { }, result::complete);
        authorization.authorize(9, "", true, "", true);
        backend.success.accept(consent());
        authorization.authorize(10, "", true, "", true);
        assertEquals(10, result.id); assertEquals(GoogleDriveAuthorization.BUSY, result.error);
        backend.returned = granted(true);
        authorization.result(Activity.RESULT_OK, new Intent());
        assertEquals(9, result.id); assertEquals(0, result.error); assertEquals(2, result.count);
    }
    @Test public void missingPlayServicesDoesNotContactTheSdk() {
        Backend backend = new Backend(); Result result = new Result();
        new GoogleDriveAuthorization(backend, ignored -> fail(), result::complete).authorize(6, "", true, "", false);
        assertNull(backend.request); assertEquals(GoogleDriveAuthorization.UNAVAILABLE, result.error);
    }
}
