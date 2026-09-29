package se.bokheim.reader.gpui;

import android.app.Service;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.ServiceConnection;
import android.os.Binder;
import android.os.IBinder;
import android.os.Parcel;
import android.os.Process;
import android.os.RemoteException;
import java.io.File;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;

/** A private, one-job process. Its taxonomy globals and SQLite handles must die
 * before the main process promotes any prepared files.
 */
public final class UpdateMigrationService extends Service {
    private static final int PID = IBinder.FIRST_CALL_TRANSACTION;
    private static final int PREPARE = PID + 1;
    private final AtomicBoolean started = new AtomicBoolean();
    private static native void nativePrepare(String job);

    private final Binder binder = new Binder() {
        @Override protected boolean onTransact(int code, Parcel data, Parcel reply, int flags) throws RemoteException {
            if (code == PID) { reply.writeInt(Process.myPid()); return true; }
            if (code != PREPARE || !started.compareAndSet(false, true)) return false;
            String path = data.readString();
            new Thread(() -> {
                try {
                    File root = new File(getFilesDir(), "updates/generations").getCanonicalFile();
                    File job = new File(path).getCanonicalFile();
                    if (!job.toPath().startsWith(root.toPath()) || !job.getName().equals("prepare-job.json")) {
                        throw new IllegalArgumentException("Invalid private migration job");
                    }
                    System.loadLibrary("desktop_gpui");
                    nativePrepare(job.getPath());
                } catch (Throwable error) {
                    android.util.Log.e("Bokheim", "Update migration helper failed", error);
                } finally {
                    // Binder death proves all native handles in this process closed.
                    Process.killProcess(Process.myPid());
                }
            }, "update-migration").start();
            return true;
        }
    };

    @Override public IBinder onBind(Intent intent) { return binder; }

    /** Called from the native startup worker, never Android's main thread. */
    public static void prepare(Context context, String path) throws Exception {
        if (android.os.Looper.myLooper() == android.os.Looper.getMainLooper()) {
            throw new IllegalStateException("Migration cannot block Android's main thread");
        }
        CountDownLatch exited = new CountDownLatch(1);
        AtomicInteger pid = new AtomicInteger();
        AtomicReference<IBinder> bound = new AtomicReference<>();
        AtomicBoolean requested = new AtomicBoolean();
        AtomicBoolean closed = new AtomicBoolean();
        AtomicReference<Exception> failure = new AtomicReference<>();
        ServiceConnection connection = new ServiceConnection() {
            @Override public void onServiceConnected(ComponentName name, IBinder service) {
                if (closed.get() || !requested.compareAndSet(false, true)) return;
                bound.set(service);
                try {
                    service.linkToDeath(exited::countDown, 0);
                    Parcel request = Parcel.obtain(), reply = Parcel.obtain();
                    try {
                        if (!service.transact(PID, request, reply, 0)) throw new RemoteException("Missing helper PID");
                        pid.set(reply.readInt());
                    } finally { request.recycle(); reply.recycle(); }
                    request = Parcel.obtain();
                    try {
                        request.writeString(path);
                        if (!service.transact(PREPARE, request, null, IBinder.FLAG_ONEWAY)) throw new RemoteException("Helper rejected job");
                    } finally { request.recycle(); }
                } catch (Exception error) { failure.set(error); exited.countDown(); }
            }
            @Override public void onServiceDisconnected(ComponentName name) { exited.countDown(); }
            @Override public void onNullBinding(ComponentName name) {
                failure.set(new IllegalStateException("Migration helper unavailable")); exited.countDown();
            }
            @Override public void onBindingDied(ComponentName name) {
                failure.set(new IllegalStateException("Migration helper binding died")); exited.countDown();
            }
        };
        Intent intent = new Intent(context, UpdateMigrationService.class);
        if (!context.bindService(intent, connection, Context.BIND_AUTO_CREATE)) throw new IllegalStateException("Cannot bind migration helper");
        try {
            if (!exited.await(30, TimeUnit.MINUTES)) throw new IllegalStateException("Migration helper timed out");
            if (failure.get() != null) throw failure.get();
            // Rust reads the atomically written result; death without a result fails.
        } finally {
            closed.set(true);
            int child = pid.get();
            IBinder service = bound.get();
            if (service != null && service.isBinderAlive() && child > 0 && child != Process.myPid()) Process.killProcess(child);
            context.unbindService(connection);
            context.stopService(intent);
        }
    }
}
