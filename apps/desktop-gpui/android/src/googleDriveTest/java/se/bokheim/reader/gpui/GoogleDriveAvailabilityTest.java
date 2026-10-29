package se.bokheim.reader.gpui;

import static org.junit.Assert.*;
import android.content.Context;
import com.google.android.gms.common.ConnectionResult;
import com.google.android.gms.common.GoogleApiAvailability;
import org.junit.Before;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.RuntimeEnvironment;
import org.robolectric.annotation.Config;
import org.robolectric.annotation.Implementation;
import org.robolectric.annotation.Implements;
import org.robolectric.shadow.api.Shadow;

@RunWith(RobolectricTestRunner.class)
@Config(sdk = 30, shadows = GoogleDriveAvailabilityTest.Availability.class)
public class GoogleDriveAvailabilityTest {
    @Implements(GoogleApiAvailability.class)
    public static class Availability {
        static int status;
        static boolean failed;
        @Implementation protected static GoogleApiAvailability getInstance() {
            return Shadow.newInstanceOf(GoogleApiAvailability.class);
        }
        @Implementation protected int isGooglePlayServicesAvailable(Context context) {
            if (failed) throw new IllegalStateException("Play services unavailable");
            return status;
        }
    }

    @Before public void reset() {
        Availability.status = ConnectionResult.SUCCESS;
        Availability.failed = false;
    }

    @Test public void onlyUsablePlayServicesEnableDrive() {
        Context context = RuntimeEnvironment.getApplication();
        assertTrue(GoogleDriveAvailability.isAvailable(context));
        for (int status : new int[] { ConnectionResult.SERVICE_MISSING, ConnectionResult.SERVICE_DISABLED,
                ConnectionResult.SERVICE_VERSION_UPDATE_REQUIRED, ConnectionResult.SERVICE_INVALID,
                ConnectionResult.SERVICE_UPDATING }) {
            Availability.status = status;
            assertFalse("Availability status " + status, GoogleDriveAvailability.isAvailable(context));
        }
    }

    @Test public void availabilityIsRecheckedAfterSettingsChanges() {
        Context context = RuntimeEnvironment.getApplication();
        Availability.status = ConnectionResult.SERVICE_DISABLED;
        assertFalse(GoogleDriveAvailability.isAvailable(context));
        Availability.status = ConnectionResult.SUCCESS;
        assertTrue(GoogleDriveAvailability.isAvailable(context));
        Availability.status = ConnectionResult.SERVICE_MISSING;
        assertFalse(GoogleDriveAvailability.isAvailable(context));
    }

    @Test public void anSdkFailureLeavesDriveDisabled() {
        Availability.failed = true;
        assertFalse(GoogleDriveAvailability.isAvailable(RuntimeEnvironment.getApplication()));
    }
}
