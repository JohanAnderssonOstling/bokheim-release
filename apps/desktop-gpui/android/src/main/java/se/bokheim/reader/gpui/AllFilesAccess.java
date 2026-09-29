package se.bokheim.reader.gpui;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ActivityNotFoundException;
import android.content.Intent;
import android.content.SharedPreferences;
import android.net.Uri;
import android.os.Build;
import android.os.Environment;
import android.provider.Settings;
import android.widget.Toast;

final class AllFilesAccess {
    private static final String PREFERENCES = "storage_permissions";
    private static final String PROMPT_HANDLED = "all_files_prompt_handled";

    private AllFilesAccess() {}

    static boolean isGranted() {
        return Build.VERSION.SDK_INT >= Build.VERSION_CODES.R && Environment.isExternalStorageManager();
    }

    static AlertDialog promptForLaunch(Activity activity, Intent intent) {
        return intent != null && Intent.ACTION_VIEW.equals(intent.getAction()) ? null : promptIfNeeded(activity);
    }

    static AlertDialog promptIfNeeded(Activity activity) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R || isGranted()) {
            return null;
        }
        SharedPreferences preferences = activity.getSharedPreferences(PREFERENCES, Activity.MODE_PRIVATE);
        if (preferences.getBoolean(PROMPT_HANDLED, false)) {
            return null;
        }
        Runnable remember = () -> preferences.edit().putBoolean(PROMPT_HANDLED, true).apply();
        return new AlertDialog.Builder(activity)
                .setTitle("Allow file access")
                .setMessage("Allow Bokheim to access and manage files in shared storage, including Documents. You can also continue using the system file picker.")
                .setPositiveButton("Open settings", (dialog, which) -> {
                    remember.run();
                    openSettings(activity);
                })
                .setNegativeButton("Not now", (dialog, which) -> remember.run())
                .setOnCancelListener(dialog -> remember.run())
                .show();
    }

    static void openSettings(Activity activity) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
            Toast.makeText(activity, "Use the file picker to select folders on this Android version.", Toast.LENGTH_LONG).show();
            return;
        }
        try {
            activity.startActivity(new Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                    Uri.parse("package:" + activity.getPackageName())));
        } catch (ActivityNotFoundException unavailable) {
            try {
                activity.startActivity(new Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION));
            } catch (ActivityNotFoundException missingSettings) {
                Toast.makeText(activity, "File access settings are unavailable on this device. You can still use the file picker.", Toast.LENGTH_LONG).show();
            }
        }
    }
}
