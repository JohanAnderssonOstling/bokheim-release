package se.bokheim.reader.gpui;

import static org.junit.Assert.*;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.content.pm.ServiceInfo;
import android.os.IBinder;
import android.os.Parcel;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.Robolectric;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.RuntimeEnvironment;
import org.robolectric.annotation.Config;

@RunWith(RobolectricTestRunner.class)
@Config(sdk = 30)
public class UpdateMigrationServiceTest {
    @Test public void migrationAndRestartArePrivateSeparateProcesses() throws Exception {
        Context context = RuntimeEnvironment.getApplication();
        ServiceInfo service = context.getPackageManager().getServiceInfo(new ComponentName(context, UpdateMigrationService.class), 0);
        ActivityInfo restart = context.getPackageManager().getActivityInfo(new ComponentName(context, UpdateRestartActivity.class), 0);
        assertFalse(service.exported);
        assertFalse(restart.exported);
        assertTrue(service.processName.endsWith(":update_migration"));
        assertTrue(restart.processName.endsWith(":update_restart"));
        assertNotEquals(service.processName, context.getApplicationInfo().processName);
    }

    @Test public void refusesToBlockAndroidMainThread() {
        assertThrows(IllegalStateException.class, () -> UpdateMigrationService.prepare(RuntimeEnvironment.getApplication(), "/unused"));
    }

    @Test public void binderReportsProcessIdentityWithoutStartingMigration() throws Exception {
        org.robolectric.android.controller.ServiceController<UpdateMigrationService> controller = Robolectric.buildService(UpdateMigrationService.class).create();
        try {
            IBinder binder = controller.get().onBind(new Intent());
            Parcel request = Parcel.obtain(), reply = Parcel.obtain();
            try {
                assertTrue(binder.transact(IBinder.FIRST_CALL_TRANSACTION, request, reply, 0));
                assertEquals(android.os.Process.myPid(), reply.readInt());
            } finally { request.recycle(); reply.recycle(); }
        } finally { controller.destroy(); }
    }
}
