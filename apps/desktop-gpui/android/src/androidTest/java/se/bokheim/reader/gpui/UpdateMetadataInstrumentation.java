package se.bokheim.reader.gpui;

import android.app.Activity;
import android.app.Instrumentation;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.content.pm.Signature;
import android.os.Build;
import android.os.Bundle;
import java.security.MessageDigest;
import java.util.Arrays;
import org.json.JSONArray;
import org.json.JSONObject;

/** Runs only from the separate test APK on a dedicated release-test device. */
public final class UpdateMetadataInstrumentation extends Instrumentation {
    @Override public void onCreate(Bundle arguments) {
        super.onCreate(arguments);
        start();
    }

    @Override public void onStart() {
        Bundle result = new Bundle();
        try {
            String packageName = getTargetContext().getPackageName();
            PackageManager manager = getTargetContext().getPackageManager();
            PackageInfo installed = manager.getPackageInfo(packageName,
                Build.VERSION.SDK_INT >= 28 ? PackageManager.GET_SIGNING_CERTIFICATES : PackageManager.GET_SIGNATURES);
            Signature[] signatures = Build.VERSION.SDK_INT >= 28
                ? installed.signingInfo.getApkContentsSigners() : installed.signatures;
            if (signatures == null || signatures.length == 0) throw new IllegalStateException("APK has no signing identity");
            String[] fingerprints = new String[signatures.length];
            for (int i = 0; i < signatures.length; i++) {
                byte[] digest = MessageDigest.getInstance("SHA-256").digest(signatures[i].toByteArray());
                StringBuilder hex = new StringBuilder();
                for (byte value : digest) hex.append(String.format(java.util.Locale.ROOT, "%02x", value & 255));
                fingerprints[i] = hex.toString();
            }
            Arrays.sort(fingerprints);
            JSONObject metadata = new JSONObject(UpdateMetadata.json());
            if (!installed.versionName.equals(metadata.getString("application"))) {
                throw new IllegalStateException("APK and native application versions differ");
            }
            JSONObject android = new JSONObject();
            android.put("package", packageName);
            android.put("debuggable", (installed.applicationInfo.flags & ApplicationInfo.FLAG_DEBUGGABLE) != 0);
            android.put("version_code", Build.VERSION.SDK_INT >= 28 ? installed.getLongVersionCode() : installed.versionCode);
            android.put("signing_cert_sha256", new JSONArray(Arrays.asList(fingerprints)));
            metadata.put("android", android);
            result.putString("bokheim_update_info", metadata.toString());
            finish(Activity.RESULT_OK, result);
        } catch (Throwable error) {
            result.putString("bokheim_update_error", error.toString());
            finish(Activity.RESULT_CANCELED, result);
        }
    }
}
