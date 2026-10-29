package se.bokheim.reader.gpui;

import static org.junit.Assert.*;
import android.content.Context;
import android.content.pm.PackageManager;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.RuntimeEnvironment;
import org.robolectric.annotation.Config;

@RunWith(RobolectricTestRunner.class)
@Config(sdk = 30)
public class GoogleDriveWithoutPlayServicesTest {
    @Test public void missingPlayServicesDisableDriveThroughTheRealSdkCheck() {
        Context context = RuntimeEnvironment.getApplication();
        assertThrows(PackageManager.NameNotFoundException.class,
            () -> context.getPackageManager().getPackageInfo("com.google.android.gms", 0));
        assertFalse(MainActivity.googleDriveAvailable(context));
    }
}
