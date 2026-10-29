package se.bokheim.reader.gpui;

import static org.junit.Assert.*;
import static org.junit.Assume.assumeTrue;
import static org.robolectric.Shadows.shadowOf;

import android.app.Activity;
import android.content.ComponentName;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.Robolectric;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.RuntimeEnvironment;
import org.robolectric.android.controller.ActivityController;
import org.robolectric.annotation.Config;

@RunWith(RobolectricTestRunner.class)
@Config(sdk = 30, shadows = AndroidLibraryStorageTest.PermissionEnvironment.class)
public class AndroidDistributionTest {
    @Test public void mergedManifestKeepsRestrictedPermissionsOnlyForDirectBuilds() throws Exception {
        android.content.Context context = RuntimeEnvironment.getApplication();
        PackageInfo info = context.getPackageManager().getPackageInfo(context.getPackageName(), PackageManager.GET_PERMISSIONS);
        List<String> permissions = info.requestedPermissions == null ? Collections.emptyList() : Arrays.asList(info.requestedPermissions);
        assertEquals(MainActivity.isSelfUpdateEnabled(), permissions.contains("android.permission.REQUEST_INSTALL_PACKAGES"));
        assertEquals(!BuildConfig.PLAY_STORE, permissions.contains("android.permission.MANAGE_EXTERNAL_STORAGE"));
    }

    @Test public void playManifestDoesNotExposeTheApkUpdateComponents() throws Exception {
        assumeTrue(!MainActivity.isSelfUpdateEnabled());
        android.content.Context context = RuntimeEnvironment.getApplication();
        PackageManager manager = context.getPackageManager();
        assertThrows(PackageManager.NameNotFoundException.class,
                () -> manager.getServiceInfo(new ComponentName(context, UpdateMigrationService.class), 0));
        assertThrows(PackageManager.NameNotFoundException.class,
                () -> manager.getActivityInfo(new ComponentName(context, UpdateRestartActivity.class), 0));
        assertNull(manager.resolveContentProvider(context.getPackageName() + ".updates", 0));
    }

    @Test public void playCannotUseOrRequestAllFilesAccessEvenAfterAnOldGrant() {
        assumeTrue(BuildConfig.PLAY_STORE);
        AndroidLibraryStorageTest.PermissionEnvironment.granted = true;
        try (ActivityController<Activity> controller = Robolectric.buildActivity(Activity.class).setup()) {
            Activity activity = controller.get();
            assertFalse(AllFilesAccess.isGranted());
            assertNull(AllFilesAccess.promptIfNeeded(activity));
            AllFilesAccess.openSettings(activity);
            assertNull(shadowOf(activity).getNextStartedActivity());
        } finally {
            AndroidLibraryStorageTest.PermissionEnvironment.granted = false;
        }
    }

    @Test public void playRejectsEveryNativeUpdaterCommandBeforeDoingAnyWork() {
        assumeTrue(!MainActivity.isSelfUpdateEnabled());
        for (String action : new String[]{"prepare", "permission", "install", "grantPermission", "restart"}) {
            assertThrows(UnsupportedOperationException.class,
                    () -> MainActivity.updateCommand(RuntimeEnvironment.getApplication(), "{\"action\":\"" + action + "\"}"));
        }
    }
}
