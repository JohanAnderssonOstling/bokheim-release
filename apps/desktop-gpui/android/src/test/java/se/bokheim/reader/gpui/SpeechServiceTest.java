package se.bokheim.reader.gpui;

import static org.junit.Assert.*;
import static org.robolectric.Shadows.shadowOf;
import android.app.Notification;
import android.app.NotificationManager;
import android.content.Intent;
import android.media.AudioManager;
import android.media.session.MediaSession;
import android.os.Looper;
import android.os.PowerManager;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import org.junit.After;
import org.junit.Before;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.Robolectric;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.android.controller.ServiceController;
import org.robolectric.annotation.Config;
import org.robolectric.annotation.LooperMode;

@RunWith(RobolectricTestRunner.class)
@Config(sdk = 28)
@LooperMode(LooperMode.Mode.PAUSED)
public class SpeechServiceTest {
    private ServiceController<SpeechService> controller;
    private SpeechService service;
    private MediaSession session;
    private MediaSession.Callback controls;
    private PowerManager.WakeLock wake;
    private final List<String> commands = new ArrayList<>();
    private boolean paused, stopped;

    @Before public void setup() throws Exception {
        paused = stopped = false;
        SpeechService.stateProvider = id -> stopped ? "" : "{\"id\":" + id + ",\"title\":\"An English book\",\"paused\":" + paused + ",\"preparing\":false,\"rate\":1.25}";
        SpeechService.controlSender = (id, action) -> {
            if (stopped) return false;
            commands.add(action);
            if (action.equals("pause")) paused = true;
            if (action.equals("resume")) paused = false;
            if (action.equals("stop")) stopped = true;
            return true;
        };
        controller = Robolectric.buildService(SpeechService.class).create();
        service = controller.get();
        java.lang.reflect.Field field = SpeechService.class.getDeclaredField("session");
        field.setAccessible(true);
        session = (MediaSession) field.get(service);
        field = SpeechService.class.getDeclaredField("controls");
        field.setAccessible(true);
        controls = (MediaSession.Callback) field.get(service);
        field = SpeechService.class.getDeclaredField("wakeLock");
        field.setAccessible(true);
        wake = (PowerManager.WakeLock) field.get(service);
    }

    @After public void teardown() { controller.destroy(); }

    @Test public void activeNarrationHasForegroundNotificationAndMediaSession() {
        service.begin(12);
        assertTrue(session.isActive());
        assertTrue(wake.isHeld());
        Notification notification = shadowOf(service).getLastForegroundNotification();
        assertEquals("An English book", notification.extras.getString(Notification.EXTRA_TITLE));
        assertNotNull(notification.extras.getParcelable(Notification.EXTRA_MEDIA_SESSION));
        assertEquals(3, notification.actions.length);
    }

    @Test public void titleAuthorChapterAndArtworkArePublishedWithoutAStopButton() throws Exception {
        android.graphics.Bitmap image = android.graphics.Bitmap.createBitmap(2, 2, android.graphics.Bitmap.Config.ARGB_8888);
        java.io.ByteArrayOutputStream output = new java.io.ByteArrayOutputStream();
        image.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, output);
        org.json.JSONArray cover = new org.json.JSONArray();
        for (byte value : output.toByteArray()) cover.put(value & 255);
        String state = new org.json.JSONObject().put("title", "Book").put("author", "Author").put("chapter", "Chapter 4 · Martha")
            .put("cover", cover).put("paused", false).toString();
        SpeechService.stateProvider = id -> state;
        service.begin(12);
        Notification notification = shadowOf(service.getSystemService(NotificationManager.class)).getNotification(SpeechService.NOTIFICATION_ID);
        assertEquals("Author", notification.extras.getString(Notification.EXTRA_TEXT));
        assertEquals("Chapter 4 · Martha", notification.extras.getString(Notification.EXTRA_SUB_TEXT));
        assertEquals(3, notification.actions.length);
        assertNotNull(notification.getLargeIcon());
    }

    @Test public void systemMediaButtonsPauseResumeAndChangeReadingItems() {
        service.begin(12);
        // Robolectric does not deliver platform MediaController binder calls.
        // Exercise the callback actually registered with this media session;
        // on-device validation covers the OS/controller boundary.
        controls.onPause();
        shadowOf(Looper.getMainLooper()).idle();
        assertTrue(paused);
        assertFalse(wake.isHeld());
        assertEquals("Play", shadowOf(service.getSystemService(NotificationManager.class)).getNotification(SpeechService.NOTIFICATION_ID).actions[1].title);
        controls.onPlay();
        controls.onSkipToPrevious();
        controls.onSkipToNext();
        shadowOf(Looper.getMainLooper()).idle();
        assertFalse(paused);
        assertTrue(wake.isHeld());
        assertTrue(commands.containsAll(java.util.Arrays.asList("pause", "resume", "back", "forward")));
    }

    @Test public void stopRemovesNotificationAndReleasesBackgroundResources() {
        service.begin(12);
        service.command("stop");
        assertTrue(stopped);
        assertFalse(wake.isHeld());
        assertFalse(session.isActive());
        assertNull(shadowOf(service.getSystemService(NotificationManager.class)).getNotification(SpeechService.NOTIFICATION_ID));
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(300));
        assertTrue(shadowOf(service).isStoppedBySelf());
    }

    @Test public void systemCustomStopActionEndsNarration() {
        service.begin(12);
        controls.onCustomAction("stop", android.os.Bundle.EMPTY);
        assertTrue(stopped);
        assertFalse(session.isActive());
        assertFalse(wake.isHeld());
    }

    @Test public void headsetTogglePausesPreparingAudioAndIgnoresKeyUp() {
        SpeechService.stateProvider = id -> "{\"title\":\"Book\",\"paused\":" + paused + ",\"preparing\":true}";
        service.begin(12);
        Intent down = new Intent(Intent.ACTION_MEDIA_BUTTON).putExtra(Intent.EXTRA_KEY_EVENT,
            new android.view.KeyEvent(android.view.KeyEvent.ACTION_DOWN, android.view.KeyEvent.KEYCODE_HEADSETHOOK));
        assertTrue(controls.onMediaButtonEvent(down));
        assertTrue(paused);
        Intent up = new Intent(Intent.ACTION_MEDIA_BUTTON).putExtra(Intent.EXTRA_KEY_EVENT,
            new android.view.KeyEvent(android.view.KeyEvent.ACTION_UP, android.view.KeyEvent.KEYCODE_HEADSETHOOK));
        assertTrue(controls.onMediaButtonEvent(up));
        assertTrue(paused);
        assertTrue(controls.onMediaButtonEvent(down));
        assertFalse(paused);
    }

    @Test public void headphoneDisconnectionPausesWithoutLosingTheNotification() {
        service.begin(12);
        service.sendBroadcast(new Intent(AudioManager.ACTION_AUDIO_BECOMING_NOISY));
        shadowOf(Looper.getMainLooper()).idle();
        assertTrue(paused);
        assertTrue(session.isActive());
        assertNotNull(shadowOf(service.getSystemService(NotificationManager.class)).getNotification(SpeechService.NOTIFICATION_ID));
    }

    @Test public void losingAudioFocusPausesSpeechAndReleasesTheWakeLock() {
        service.begin(12);
        service.focusChanged(AudioManager.AUDIOFOCUS_LOSS_TRANSIENT);
        assertTrue(paused);
        assertFalse(wake.isHeld());
        assertTrue(commands.contains("pause"));
    }

    @Test public void deniedResumeKeepsTheNotificationPaused() {
        service.begin(12);
        service.command("pause");
        shadowOf(service.getSystemService(AudioManager.class)).setNextFocusRequestResponse(AudioManager.AUDIOFOCUS_REQUEST_FAILED);
        service.command("resume");
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(300));
        assertTrue(paused);
        assertFalse(wake.isHeld());
        assertEquals("Play", shadowOf(service.getSystemService(NotificationManager.class)).getNotification(SpeechService.NOTIFICATION_ID).actions[1].title);
    }

    @Test public void staleNotificationCannotControlAReplacementNarration() {
        service.begin(12);
        service.onStartCommand(new Intent(service, SpeechService.class).setAction("stop").putExtra("id", 9), 0, 1);
        assertFalse(stopped);
        assertTrue(session.isActive());
    }

    @Test public void coldNotificationCannotStartNarrationOrAcceptAFilePath() {
        service.onStartCommand(new Intent(service, SpeechService.class).setAction("resume").putExtra("id", 12).putExtra("path", "/private/file"), 0, 1);
        assertTrue(commands.isEmpty());
        assertFalse(session.isActive());
        assertFalse(wake.isHeld());
        assertTrue(shadowOf(service).isStoppedBySelf());
    }

    @Test public void expiredNativeSessionIsRemovedByStopSignal() {
        service.begin(12);
        stopped = true;
        SpeechService.dispatch(service, "{\"command\":\"stop\",\"id\":12}");
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(300));
        assertFalse(session.isActive());
        assertFalse(wake.isHeld());
    }

    @Test public void stateChangesUpdateNotificationsWithoutPeriodicPolling() {
        java.util.concurrent.atomic.AtomicInteger reads = new java.util.concurrent.atomic.AtomicInteger();
        java.util.function.LongFunction<String> state = SpeechService.stateProvider;
        SpeechService.stateProvider = id -> { reads.incrementAndGet(); return state.apply(id); };
        service.begin(12);
        int initial = reads.get();
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMinutes(1));
        assertEquals(initial, reads.get());
        paused = true; // A reader-side change, without a notification command.
        SpeechService.dispatch(service, "{\"command\":\"update\",\"id\":12}");
        shadowOf(Looper.getMainLooper()).idle();
        assertEquals(initial + 1, reads.get());
        assertFalse(wake.isHeld());
        assertEquals("Play", shadowOf(service.getSystemService(NotificationManager.class)).getNotification(SpeechService.NOTIFICATION_ID).actions[1].title);
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMinutes(1));
        assertEquals(initial + 1, reads.get());
    }

    @Test public void staleStateSignalsCannotChangeTheCurrentSession() {
        service.begin(12);
        paused = true;
        SpeechService.dispatch(service, "{\"command\":\"update\",\"id\":9}");
        shadowOf(Looper.getMainLooper()).idle();
        assertTrue(session.isActive());
        assertTrue(wake.isHeld());
        assertEquals("Pause", shadowOf(service.getSystemService(NotificationManager.class)).getNotification(SpeechService.NOTIFICATION_ID).actions[1].title);
    }
}
