package se.bokheim.reader.gpui;

import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import java.util.function.Consumer;

/** Keeps the Activity bridge available without linking the Google SDK. */
final class GoogleDriveAuthorization {
    static final int UNAVAILABLE = 4;
    interface Completion { void complete(long id, String token, int error); }
    private final Completion completion;

    GoogleDriveAuthorization(Context context, Consumer<PendingIntent> launch, Completion completion) {
        this.completion = completion;
    }

    void authorize(long id, String email, boolean interactive, String rejectedToken, boolean available) {
        completion.complete(id, null, UNAVAILABLE);
    }

    void result(int resultCode, Intent data) {}
    void destroy() {}
}
