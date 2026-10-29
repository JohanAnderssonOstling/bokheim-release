package se.bokheim.reader.gpui;

import android.content.Context;
import android.util.Log;
import com.google.android.gms.common.ConnectionResult;
import com.google.android.gms.common.GoogleApiAvailability;

/** Google Drive is optional; missing or unusable Play services leave it disabled. */
final class GoogleDriveAvailability {
    private GoogleDriveAvailability() {}

    static boolean isAvailable(Context context) {
        try {
            return GoogleApiAvailability.getInstance().isGooglePlayServicesAvailable(context)
                == ConnectionResult.SUCCESS;
        } catch (RuntimeException | LinkageError error) {
            Log.w("Bokheim", "Google Play services availability could not be checked", error);
            return false;
        }
    }
}
