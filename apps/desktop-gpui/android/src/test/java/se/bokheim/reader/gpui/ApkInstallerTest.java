package se.bokheim.reader.gpui;

import static org.junit.Assert.*;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageInfo;
import android.provider.Settings;
import java.io.File;
import java.io.IOException;
import java.nio.file.Files;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.RuntimeEnvironment;
import org.robolectric.annotation.Config;

@RunWith(RobolectricTestRunner.class)
@Config(sdk = 30)
public class ApkInstallerTest {
    private static final String HASH = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    @Test public void verifiesExactBytesAndRejectsCorruption() throws Exception {
        File root = Files.createTempDirectory("apk-stage").toFile();
        File apk = new File(root, "application.bin");
        Files.write(apk.toPath(), new byte[] {97, 98, 99});
        ApkInstaller.verifyBytes(root, apk, 3, HASH);
        assertThrows(IOException.class, () -> ApkInstaller.verifyBytes(root, apk, 4, HASH));
        Files.write(apk.toPath(), new byte[] {98, 97, 100});
        assertThrows(IOException.class, () -> ApkInstaller.verifyBytes(root, apk, 3, HASH));
    }

    @Test public void rejectsSiblingPathsAndSymlinksOutsideStaging() throws Exception {
        File root = Files.createTempDirectory("apk-stage").toFile();
        File staging = new File(root, "staging");
        assertTrue(staging.mkdir());
        File external = new File(root, "application.bin");
        Files.write(external.toPath(), new byte[] {97, 98, 99});
        assertThrows(IOException.class, () -> ApkInstaller.verifyBytes(staging, external, 3, HASH));
        File link = new File(staging, "application.bin");
        Files.createSymbolicLink(link.toPath(), external.toPath());
        assertThrows(IOException.class, () -> ApkInstaller.verifyBytes(staging, link, 3, HASH));
    }

    private PackageInfo info(String name, String version, long code) {
        PackageInfo info = new PackageInfo();
        info.packageName = name;
        info.versionName = version;
        info.setLongVersionCode(code);
        return info;
    }

    @Test public void requiresMatchingPackageVersionAndIncreasingVersionCode() throws Exception {
        PackageInfo old = info("se.bokheim.reader.gpui", "0.1.6", 1006);
        ApkInstaller.verifyPackage(old.packageName, old, info(old.packageName, "0.1.7", 1007), "0.1.7");
        for (PackageInfo bad : new PackageInfo[] { null, info("other.app", "0.1.7", 1007),
            info(old.packageName, "0.1.8", 1008), info(old.packageName, "0.1.7", 1006) }) {
            assertThrows(IOException.class, () -> ApkInstaller.verifyPackage(old.packageName, old, bad, "0.1.7"));
        }
    }

    @Test public void onlyGrantsReadAccessToThePrivateUpdateFile() throws Exception {
        Context context = RuntimeEnvironment.getApplication();
        File staging = new File(context.getFilesDir(), "updates/staging/approved-plan");
        assertTrue(staging.mkdirs() || staging.isDirectory());
        File apk = new File(staging, "application.bin");
        Files.write(apk.toPath(), new byte[] {97, 98, 99});
        Intent intent = ApkInstaller.installIntent(context, apk);
        assertEquals(Intent.ACTION_INSTALL_PACKAGE, intent.getAction());
        assertEquals("content", intent.getData().getScheme());
        assertEquals(context.getPackageName() + ".updates", intent.getData().getAuthority());
        assertEquals(Intent.FLAG_GRANT_READ_URI_PERMISSION, intent.getFlags());
        assertTrue(intent.getBooleanExtra(Intent.EXTRA_RETURN_RESULT, false));
        assertEquals(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, ApkInstaller.permissionIntent(context).getAction());
        assertEquals("package:" + context.getPackageName(), ApkInstaller.permissionIntent(context).getDataString());
    }
}
