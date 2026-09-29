package se.bokheim.reader.gpui;

import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.os.Build;
import android.provider.Settings;
import androidx.core.content.FileProvider;
import java.io.File;
import java.io.FileInputStream;
import java.io.IOException;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;

/** Explicit handoff to Android's installer. The shared coordinator owns consent
 * and staging; only the installed version proves success after process replacement.
 */
final class ApkInstaller {
    private ApkInstaller() {}
    static final class InvalidPackageException extends IOException {
        InvalidPackageException(String message) { super(message); }
    }

    static boolean permissionGranted(Context context) {
        return context.getPackageManager().canRequestPackageInstalls();
    }

    static Intent permissionIntent(Context context) {
        return new Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
            Uri.parse("package:" + context.getPackageName()));
    }

    /** Run on the update worker immediately before an explicit Install action.
     * Expected metadata must come from the approved, authenticated manifest.
     */
    static File verify(Context context, File staged, String expectedVersion,
                       long expectedBytes, String expectedSha256) throws IOException {
        File root = new File(context.getFilesDir(), "updates/staging").getCanonicalFile();
        File apk = staged.getCanonicalFile();
        verifyBytes(root, apk, expectedBytes, expectedSha256);
        PackageManager manager = context.getPackageManager();
        PackageInfo candidate = manager.getPackageArchiveInfo(apk.getPath(), 0);
        final PackageInfo installed;
        try {
            installed = manager.getPackageInfo(context.getPackageName(), 0);
        } catch (PackageManager.NameNotFoundException error) {
            throw new IOException("Cannot inspect installed application", error);
        }
        verifyPackage(context.getPackageName(), installed, candidate, expectedVersion);
        return apk;
    }

    static void verifyBytes(File root, File apk, long size, String sha256) throws IOException {
        root = root.getCanonicalFile();
        apk = apk.getCanonicalFile();
        if (!apk.toPath().startsWith(root.toPath()) || apk.equals(root) || !apk.isFile()) {
            throw new IOException("Update package is outside private staging");
        }
        if (size <= 0 || apk.length() != size || sha256 == null || !sha256.matches("[0-9a-fA-F]{64}")) {
            throw new IOException("Invalid update package metadata");
        }
        try (FileInputStream input = new FileInputStream(apk)) {
            MessageDigest digest = MessageDigest.getInstance("SHA-256");
            byte[] buffer = new byte[65536];
            long count = 0;
            int n;
            while ((n = input.read(buffer)) != -1) {
                count += n;
                if (count > size) throw new IOException("Update package grew during verification");
                digest.update(buffer, 0, n);
            }
            StringBuilder actual = new StringBuilder(64);
            for (byte b : digest.digest()) actual.append(String.format(java.util.Locale.ROOT, "%02x", b & 255));
            if (count != size || !actual.toString().equalsIgnoreCase(sha256)) {
                throw new IOException("Update package failed verification");
            }
        } catch (NoSuchAlgorithmException impossible) {
            throw new IOException("SHA-256 unavailable", impossible);
        }
    }

    static void verifyPackage(String packageName, PackageInfo installed,
                              PackageInfo candidate, String expectedVersion) throws IOException {
        if (candidate == null || !packageName.equals(candidate.packageName)
            || expectedVersion == null || !expectedVersion.equals(candidate.versionName)
            || versionCode(candidate) <= versionCode(installed)) {
            throw new InvalidPackageException("APK is not the approved newer version of this application");
        }
        // Android verifies the APK signing certificate against the installed app.
        // A successful activity result alone does not authorize data migrations.
    }

    private static long versionCode(PackageInfo info) {
        return Build.VERSION.SDK_INT >= 28 ? info.getLongVersionCode() : info.versionCode;
    }

    /** Call only for a foreground user action, after verification on the worker.
     * A cancelled installer leaves the approved staged package available to retry.
     */
    static Intent installIntent(Context context, File verifiedApk) {
        Uri uri = FileProvider.getUriForFile(context, context.getPackageName() + ".updates", verifiedApk);
        return new Intent(Intent.ACTION_INSTALL_PACKAGE)
            .setDataAndType(uri, "application/vnd.android.package-archive")
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            .putExtra(Intent.EXTRA_RETURN_RESULT, true);
    }
}
