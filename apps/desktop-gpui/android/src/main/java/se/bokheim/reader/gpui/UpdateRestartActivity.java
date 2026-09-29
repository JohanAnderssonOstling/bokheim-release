package se.bokheim.reader.gpui;

/** Runs only after an explicit Restart action, outside the process being stopped. */
public final class UpdateRestartActivity extends android.app.Activity {
    @Override public void onCreate(android.os.Bundle state) {
        super.onCreate(state);
        int oldPid = getIntent().getIntExtra("old_pid", 0);
        if (oldPid > 0 && oldPid != android.os.Process.myPid()) android.os.Process.killProcess(oldPid);
        android.content.Intent launch = getPackageManager().getLaunchIntentForPackage(getPackageName());
        if (launch != null) startActivity(launch.addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK | android.content.Intent.FLAG_ACTIVITY_CLEAR_TASK));
        finish();
        android.os.Process.killProcess(android.os.Process.myPid());
    }
}
