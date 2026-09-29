package se.bokheim.reader.gpui;

import static org.junit.Assert.*;
import static org.robolectric.Shadows.shadowOf;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.Intent;
import android.os.Environment;
import android.os.Looper;
import android.provider.Settings;
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
import org.robolectric.shadows.ShadowAlertDialog;
import org.robolectric.shadows.ShadowEnvironment;

@RunWith(RobolectricTestRunner.class)
@Config(sdk = 30, shadows = AllFilesAccessTest.PermissionEnvironment.class)
public class AllFilesAccessTest {
    @Implements(Environment.class)
    public static class PermissionEnvironment extends ShadowEnvironment {
        static boolean granted;

        @Implementation(minSdk = 30)
        protected static boolean isExternalStorageManager() {
            return granted;
        }
    }
    private ActivityController<Activity> controller;
    private Activity activity;

    @Before public void setUp() {
        PermissionEnvironment.granted = false;
        controller = Robolectric.buildActivity(Activity.class).setup();
        activity = controller.get();
        activity.getSharedPreferences("storage_permissions", Activity.MODE_PRIVATE).edit().clear().commit();
    }

    @After public void tearDown() {
        controller.pause().stop().destroy();
    }

    @Test public void acceptingOpensSettingsForBokheim() {
        AllFilesAccess.promptIfNeeded(activity);
        AlertDialog dialog = ShadowAlertDialog.getLatestAlertDialog();
        assertNotNull(dialog);
        dialog.getButton(AlertDialog.BUTTON_POSITIVE).performClick();
        shadowOf(Looper.getMainLooper()).idle();
        Intent intent = shadowOf(activity).getNextStartedActivity();
        assertEquals(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, intent.getAction());
        assertEquals("package:" + activity.getPackageName(), intent.getDataString());
    }

    @Test public void decliningDoesNotLaunchSettingsOrPromptAgain() {
        AllFilesAccess.promptIfNeeded(activity);
        AlertDialog dialog = ShadowAlertDialog.getLatestAlertDialog();
        dialog.getButton(AlertDialog.BUTTON_NEGATIVE).performClick();
        shadowOf(Looper.getMainLooper()).idle();
        assertNull(shadowOf(activity).getNextStartedActivity());
        AllFilesAccess.promptIfNeeded(activity);
        assertSame(dialog, ShadowAlertDialog.getLatestAlertDialog());
        assertFalse(dialog.isShowing());
    }

    @Test public void cancellingDoesNotPromptAgain() {
        AllFilesAccess.promptIfNeeded(activity);
        AlertDialog dialog = ShadowAlertDialog.getLatestAlertDialog();
        dialog.cancel();
        shadowOf(Looper.getMainLooper()).idle();
        AllFilesAccess.promptIfNeeded(activity);
        assertSame(dialog, ShadowAlertDialog.getLatestAlertDialog());
        assertFalse(dialog.isShowing());
    }

    @Test @Config(sdk = 28) public void olderAndroidDoesNotPrompt() {
        AllFilesAccess.promptIfNeeded(activity);
        assertNull(ShadowAlertDialog.getLatestAlertDialog());
        assertNull(shadowOf(activity).getNextStartedActivity());
    }

    @Test public void launcherPromptsAfterOpeningAnExternalBook() {
        assertNull(AllFilesAccess.promptForLaunch(activity, new Intent(Intent.ACTION_VIEW)));
        AlertDialog dialog = AllFilesAccess.promptForLaunch(activity, new Intent(Intent.ACTION_MAIN));
        assertNotNull(dialog);
        assertTrue(dialog.isShowing());
        dialog.dismiss();
    }

    @Test public void explicitRetryWorksAfterDeclining() {
        AlertDialog dialog = AllFilesAccess.promptIfNeeded(activity);
        dialog.getButton(AlertDialog.BUTTON_NEGATIVE).performClick();
        shadowOf(Looper.getMainLooper()).idle();
        assertNull(AllFilesAccess.promptIfNeeded(activity));
        AllFilesAccess.openSettings(activity);
        assertEquals(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                shadowOf(activity).getNextStartedActivity().getAction());
    }

    @Test public void localFolderMustStayInsideStorageVolume() throws Exception {
        java.io.File root = new java.io.File(activity.getCacheDir(), "volume");
        java.io.File child = new java.io.File(root, "Documents/Bokheim");
        assertTrue(child.mkdirs() || child.isDirectory());
        assertEquals(child.getCanonicalFile(), AndroidLibraryStorage.accessibleChild(root, "Documents/Bokheim"));
        assertNull(AndroidLibraryStorage.accessibleChild(root, "missing"));
        assertThrows(java.io.IOException.class, () -> AndroidLibraryStorage.accessibleChild(root, "../outside"));
    }

    @Test public void grantedLocalTreeUsesItsFilesystemDirectory() throws Exception {
        PermissionEnvironment.granted = true;
        java.io.File folder = new java.io.File(Environment.getExternalStorageDirectory(), "Documents/Bokheim");
        assertTrue(folder.mkdirs() || folder.isDirectory());
        android.net.Uri tree = android.provider.DocumentsContract.buildTreeDocumentUri(
                "com.android.externalstorage.documents", "primary:Documents/Bokheim");
        assertEquals(folder.getCanonicalFile(), AndroidLibraryStorage.localDirectory(activity, tree));
    }

    @Test public void providersAndDeniedAccessRetainSaf() throws Exception {
        android.net.Uri local = android.net.Uri.parse("content://com.android.externalstorage.documents/tree/primary%3ADocuments");
        assertNull(AndroidLibraryStorage.localDirectory(activity, local));
        PermissionEnvironment.granted = true;
        assertNull(AndroidLibraryStorage.localDirectory(activity,
                android.net.Uri.parse("content://example.cloud.documents/tree/books")));
    }

    @Test public void alreadyGrantedDoesNotPrompt() {
        PermissionEnvironment.granted = true;
        AllFilesAccess.promptIfNeeded(activity);
        assertNull(ShadowAlertDialog.getLatestAlertDialog());
        assertNull(shadowOf(activity).getNextStartedActivity());
    }
}
