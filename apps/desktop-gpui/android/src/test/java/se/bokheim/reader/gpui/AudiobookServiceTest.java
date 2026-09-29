package se.bokheim.reader.gpui;

import static org.junit.Assert.*;
import static org.robolectric.Shadows.shadowOf;

import android.content.Intent;
import android.os.Looper;
import androidx.media3.exoplayer.ExoPlayer;
import org.json.JSONObject;
import org.junit.After;
import org.junit.Before;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.Robolectric;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.android.controller.ServiceController;
import org.robolectric.annotation.Config;
import org.robolectric.annotation.LooperMode;
import java.util.ArrayList;
import java.util.List;

@RunWith(RobolectricTestRunner.class)
@Config(sdk = 28)
@LooperMode(LooperMode.Mode.PAUSED)
public class AudiobookServiceTest {
    private ServiceController<AudiobookService> controller;
    private AudiobookService service;
    private ExoPlayer player;
    private final List<JSONObject> events = new ArrayList<>();
    private final List<Long> interruptedSources = new ArrayList<>();

    @Before public void setUp() throws Exception {
        AudiobookService.sourceInterrupt = interruptedSources::add;
        AudiobookService.stateListener = state -> {
            try { events.add(new JSONObject(state)); }
            catch (Exception error) { throw new AssertionError(error); }
        };
        controller = Robolectric.buildService(AudiobookService.class).create();
        service = controller.get();
        java.lang.reflect.Field field = AudiobookService.class.getDeclaredField("player");
        field.setAccessible(true);
        player = (ExoPlayer) field.get(service);
    }
    @After public void tearDown() { controller.destroy(); }

    @Test public void cancelledBlockedLoaderDoesNotReportPlaybackFailure() throws Exception {
        class BlockedRead implements androidx.media3.exoplayer.upstream.Loader.Loadable {
            final java.util.concurrent.CountDownLatch entered = new java.util.concurrent.CountDownLatch(1);
            final java.util.concurrent.CountDownLatch interrupted = new java.util.concurrent.CountDownLatch(1);
            @Override public void cancelLoad() { interrupted.countDown(); }
            @Override public void load() throws java.io.IOException {
                entered.countDown();
                try { interrupted.await(); }
                catch (InterruptedException error) { Thread.currentThread().interrupt(); }
                throw new java.io.IOException("audio read superseded");
            }
        }
        BlockedRead read = new BlockedRead();
        int[] completed = {0}, cancelled = {0}, failed = {0};
        androidx.media3.exoplayer.upstream.Loader loader = new androidx.media3.exoplayer.upstream.Loader("audio-seek-test");
        try {
            loader.startLoading(read, new androidx.media3.exoplayer.upstream.Loader.Callback<BlockedRead>() {
                @Override public void onLoadCompleted(BlockedRead load, long elapsed, long duration) { completed[0]++; }
                @Override public void onLoadCanceled(BlockedRead load, long elapsed, long duration, boolean released) { cancelled[0]++; }
                @Override public androidx.media3.exoplayer.upstream.Loader.LoadErrorAction onLoadError(BlockedRead load, long elapsed, long duration, java.io.IOException error, int count) {
                    failed[0]++;
                    return androidx.media3.exoplayer.upstream.Loader.DONT_RETRY_FATAL;
                }
            }, 3);
            assertTrue(read.entered.await(2, java.util.concurrent.TimeUnit.SECONDS));
            loader.cancelLoading();
            long deadline = System.nanoTime() + java.util.concurrent.TimeUnit.SECONDS.toNanos(2);
            while (cancelled[0] == 0 && failed[0] == 0 && System.nanoTime() < deadline) {
                shadowOf(Looper.getMainLooper()).idle();
                Thread.yield();
            }
            assertEquals(1, cancelled[0]);
            assertEquals(0, completed[0]);
            assertEquals(0, failed[0]);
        } finally { loader.release(); }
    }

    @Test public void streamingSourceReadsOnlyRequestedRangesAndSupportsReopening() throws Exception {
        List<Long> offsets = new ArrayList<>();
        AudiobookService.AudioSource source = new AudiobookService.AudioSource((id, offset, length) -> {
            assertEquals(12, id);
            offsets.add(offset);
            assertTrue(length <= 256 * 1024);
            byte[] bytes = new byte[length];
            java.util.Arrays.fill(bytes, (byte) 42);
            return bytes;
        });
        android.net.Uri uri = android.net.Uri.parse("bokheim-audio://12/8388608");
        assertEquals(16, source.open(new androidx.media3.datasource.DataSpec.Builder().setUri(uri).setPosition(8388592).build()));
        assertTrue(offsets.isEmpty());
        byte[] buffer = new byte[32];
        assertEquals(16, source.read(buffer, 4, 28));
        assertEquals(0, buffer[3]);
        assertEquals(42, buffer[4]);
        assertEquals(androidx.media3.common.C.RESULT_END_OF_INPUT, source.read(buffer, 0, 32));
        source.close();
        assertEquals(8, source.open(new androidx.media3.datasource.DataSpec.Builder().setUri(uri).setPosition(100).setLength(8).build()));
        assertEquals(8, source.read(buffer, 0, 32));
        assertEquals(java.util.Arrays.asList(8388592L, 100L), offsets);
        source.close();
        assertThrows(java.io.IOException.class, () -> source.open(new androidx.media3.datasource.DataSpec.Builder().setUri(uri).setPosition(8388609).build()));
        source.close();
    }

    @Test public void sleepTimerFadesThenPausesAndRestoresVolume() {
        load(1, 0);
        send("{\"command\":\"sleep\",\"id\":1,\"remaining\":60000}");
        long now = android.os.SystemClock.elapsedRealtime();
        service.updateSleepTimer(now + 30_000);
        assertEquals(1f, player.getVolume(), 0.001f);
        service.updateSleepTimer(now + 45_000);
        assertEquals(0.5f, player.getVolume(), 0.001f);
        service.updateSleepTimer(now + 60_000);
        assertFalse(player.getPlayWhenReady());
        assertEquals(1f, player.getVolume(), 0.001f);
    }

    @Test public void cancellingOrReplacingSleepRestoresVolume() {
        load(1, 0);
        send("{\"command\":\"sleep\",\"id\":1,\"remaining\":15000}");
        assertEquals(0.5f, player.getVolume(), 0.001f);
        send("{\"command\":\"sleep\",\"id\":1}");
        assertEquals(1f, player.getVolume(), 0.001f);
        service.updateSleepTimer(android.os.SystemClock.elapsedRealtime() + 100_000);
        assertEquals(1f, player.getVolume(), 0.001f);
        send("{\"command\":\"sleep\",\"id\":1,\"remaining\":15000}");
        send("{\"command\":\"sleep\",\"id\":1,\"remaining\":60000}");
        assertEquals(1f, player.getVolume(), 0.001f);
    }

    @Test public void chapterSleepFadeAccountsForPlaybackSpeedAndSeeking() {
        load(1, 30_000);
        send("{\"command\":\"speed\",\"id\":1,\"speed\":2}");
        send("{\"command\":\"sleep\",\"id\":1,\"chapterEnd\":60000}");
        assertEquals(0.5f, player.getVolume(), 0.001f);
        player.seekTo(0);
        service.updateSleepTimer(android.os.SystemClock.elapsedRealtime());
        assertEquals(1f, player.getVolume(), 0.001f);
        player.seekTo(60_000);
        service.updateSleepTimer(android.os.SystemClock.elapsedRealtime());
        assertFalse(player.getPlayWhenReady());
        assertEquals(1f, player.getVolume(), 0.001f);
    }

    private void shake(long now) {
        service.handleShakeAcceleration(3f, now);
        service.handleShakeAcceleration(1f, now + 100);
        service.handleShakeAcceleration(3f, now + 200);
    }

    @Test public void deliberateShakeRestartsOriginalDurationAndRejectsIncidentalMovement() throws Exception {
        load(1, 0);
        send("{\"command\":\"sleep\",\"id\":1,\"remaining\":60000}");
        long now = android.os.SystemClock.elapsedRealtime();
        service.updateSleepTimer(now + 45_000);
        assertEquals(0.5f, player.getVolume(), 0.001f);
        service.handleShakeAcceleration(1.3f, now + 45_000);
        service.handleShakeAcceleration(3f, now + 45_100);
        assertEquals(0.5f, player.getVolume(), 0.001f);
        service.handleShakeAcceleration(1f, now + 45_200);
        service.handleShakeAcceleration(3f, now + 45_300);
        assertEquals(1f, player.getVolume(), 0.001f);
        assertEquals(60000, events.get(events.size() - 1).getJSONObject("sleepReset").getLong("remaining"));
        service.updateSleepTimer(now + 90_300);
        assertEquals(0.5f, player.getVolume(), 0.001f);
    }

    @Test public void chapterShakeExtendsToNextChapterOncePerGesture() throws Exception {
        send("{\"command\":\"load\",\"id\":1,\"path\":\"/missing-test-book.m4b\",\"title\":\"Test\",\"position\":45000,\"chapterEnds\":[60000,120000,180000]}");
        send("{\"command\":\"sleep\",\"id\":1,\"chapterEnd\":60000}");
        long now = android.os.SystemClock.elapsedRealtime();
        shake(now);
        assertEquals(120000, events.get(events.size() - 1).getJSONObject("sleepReset").getLong("chapterEnd"));
        service.handleShakeAcceleration(1f, now + 300);
        shake(now + 400);
        service.updateSleepTimer(now + 700);
        player.seekTo(120000);
        service.updateSleepTimer(now + 800);
        assertFalse(player.getPlayWhenReady());
        assertEquals(1f, player.getVolume(), 0.001f);
    }

    @Test public void directPlaybackRegistersSessionForServiceNotifications() {
        assertEquals(1, service.getSessions().size());
        assertSame(player, service.getSessions().get(0).getPlayer());
    }

    @Test public void lockScreenCommandsSeekByTheirAdvertisedIntervals() throws Exception {
        load(1, 60_000);
        androidx.media3.session.MediaSession session = service.getSessions().get(0);
        AudiobookService.PlaybackControls controls = new AudiobookService.PlaybackControls();
        controls.onCustomCommand(session, null, AudiobookService.SKIP_BACK, android.os.Bundle.EMPTY).get();
        assertEquals(45_000, player.getCurrentPosition());
        controls.onCustomCommand(session, null, AudiobookService.SKIP_FORWARD, android.os.Bundle.EMPTY).get();
        assertEquals(75_000, player.getCurrentPosition());
        player.seekTo(5_000);
        controls.onCustomCommand(session, null, AudiobookService.SKIP_BACK, android.os.Bundle.EMPTY).get();
        assertEquals(0, player.getCurrentPosition());
    }

    private void send(String command) {
        AudiobookService.dispatch(service, command);
        shadowOf(Looper.getMainLooper()).idle();
    }
    private void load(long id, long position) {
        send("{\"command\":\"load\",\"id\":" + id + ",\"path\":\"/missing-test-book.m4b\",\"title\":\"Test book\",\"author\":\"Test author\",\"position\":" + position + "}");
    }
    @Test public void controlsQueuedDuringColdStartAreAppliedInOrder() throws Exception {
        controller.destroy();
        load(8, 10_000);
        send("{\"command\":\"seek\",\"id\":8,\"position\":50000,\"sequence\":1}");
        send("{\"command\":\"toggle\",\"id\":8}");
        controller = Robolectric.buildService(AudiobookService.class).create();
        service = controller.get();
        java.lang.reflect.Field field = AudiobookService.class.getDeclaredField("player");
        field.setAccessible(true);
        player = (ExoPlayer) field.get(service);
        service.onStartCommand(null, 0, 1);
        assertEquals(50_000, player.getCurrentPosition());
        assertFalse(player.getPlayWhenReady());
        assertFalse(events.stream().anyMatch(e -> e.optLong("id") == 8 && e.optBoolean("released")));
    }
    @Test public void restoresPositionBeforePlayingAndPublishesMetadata() {
        load(1, 120_000);
        assertEquals(120_000, player.getCurrentPosition());
        assertEquals("Test book", player.getCurrentMediaItem().mediaMetadata.title);
        assertEquals("Test author", player.getCurrentMediaItem().mediaMetadata.artist);
        assertEquals(15_000, player.getSeekBackIncrement());
        assertEquals(30_000, player.getSeekForwardIncrement());
        assertEquals(androidx.media3.common.C.AUDIO_CONTENT_TYPE_SPEECH, player.getAudioAttributes().contentType);
    }
    @Test public void loadAppliesSavedSpeedBeforePlayback() {
        send("{\"command\":\"load\",\"id\":20,\"path\":\"/missing-test-book.m4b\",\"title\":\"Test\",\"speed\":1.5}");
        assertEquals(1.5f, player.getPlaybackParameters().speed, 0.001f);
        assertTrue(player.getPlayWhenReady());
    }
    @Test public void bufferingIsReportedSeparatelyAndClearedByPause() {
        load(71, 30_000);
        assertTrue(events.stream().anyMatch(e -> e.optLong("id") == 71 && e.optBoolean("buffering")));
        send("{\"command\":\"toggle\",\"id\":71}");
        JSONObject paused = events.get(events.size() - 1);
        assertTrue(paused.has("buffering"));
        assertFalse(paused.optBoolean("buffering"));
        assertFalse(paused.optBoolean("playing"));
    }
    @Test public void applicationAndMediaSeeksInterruptTheCurrentSource() {
        load(73, 30_000);
        interruptedSources.clear();
        send("{\"command\":\"seek\",\"id\":73,\"position\":50000,\"sequence\":1}");
        assertEquals(java.util.Arrays.asList(73L), interruptedSources);
        interruptedSources.clear();
        player.seekBack();
        shadowOf(Looper.getMainLooper()).idle();
        assertEquals(java.util.Arrays.asList(73L), interruptedSources);
    }
    @Test public void explicitPauseWhileLoadingIsIdempotent() {
        load(72, 30_000);
        assertTrue(player.getPlayWhenReady());
        send("{\"command\":\"pause\",\"id\":72}");
        send("{\"command\":\"pause\",\"id\":72}");
        assertFalse(player.getPlayWhenReady());
        assertFalse(events.get(events.size() - 1).optBoolean("buffering"));
        assertFalse(events.get(events.size() - 1).optBoolean("requested"));
        send("{\"command\":\"pause\",\"id\":71}");
        assertFalse(player.getPlayWhenReady());
        send("{\"command\":\"toggle\",\"id\":72}");
        assertTrue(player.getPlayWhenReady());
        assertTrue(events.get(events.size() - 1).optBoolean("requested"));
    }
    @Test public void oldReaderCannotSeekOrStopReplacementBook() {
        load(1, 10_000);
        load(2, 20_000);
        send("{\"command\":\"seek\",\"id\":1,\"position\":0,\"sequence\":1}");
        send("{\"command\":\"stop\",\"id\":1}");
        assertEquals("2", player.getCurrentMediaItem().mediaId);
        assertEquals(20_000, player.getCurrentPosition());
        assertTrue(events.stream().anyMatch(e -> e.optLong("id") == 1 && e.optBoolean("released")));
    }
    @Test public void seekAndSpeedCommandsReachTheSessionPlayer() {
        load(1, 0);
        send("{\"command\":\"seek\",\"id\":1,\"position\":45000,\"sequence\":3}");
        send("{\"command\":\"speed\",\"id\":1,\"speed\":1.5}");
        assertEquals(45_000, player.getCurrentPosition());
        assertEquals(1.5f, player.getPlaybackParameters().speed, 0.001f);
        assertEquals(1.0f, player.getPlaybackParameters().pitch, 0.001f);
        assertTrue(events.stream().anyMatch(e -> e.optLong("sequence") == 3));
    }
    @Test public void headphoneDisconnectionPausesThePlayer() {
        load(1, 0);
        player.setPlayWhenReady(true);
        final int[] reason = {0};
        player.addListener(new androidx.media3.common.Player.Listener() {
            @Override public void onPlayWhenReadyChanged(boolean ready, int changeReason) { reason[0] = changeReason; }
        });
        service.sendBroadcast(new Intent(android.media.AudioManager.ACTION_AUDIO_BECOMING_NOISY));
        shadowOf(Looper.getMainLooper()).idle();
        assertFalse(player.getPlayWhenReady());
        assertEquals(androidx.media3.common.Player.PLAY_WHEN_READY_CHANGE_REASON_AUDIO_BECOMING_NOISY, reason[0]);
    }
    @Test public void externalIntentCannotSupplyPrivateSourcePath() {
        service.onStartCommand(new Intent().putExtra("se.bokheim.reader.gpui.AUDIOBOOK_COMMAND", "{\"command\":\"load\",\"id\":4,\"path\":\"/private.m4b\"}"), 0, 1);
        assertNull(player.getCurrentMediaItem());
    }
    @Test public void explicitCloseClearsTheSessionAndReleasesTheSource() {
        load(1, 30_000);
        send("{\"command\":\"stop\",\"id\":1}");
        assertEquals(0, player.getMediaItemCount());
        assertFalse(player.isPlaying());
        assertTrue(events.stream().anyMatch(e -> e.optLong("id") == 1 && e.optBoolean("released")));
    }
}
