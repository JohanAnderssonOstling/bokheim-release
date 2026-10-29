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


    private ChapterPlayer sessionPlayer() throws Exception {
        java.lang.reflect.Field field = AudiobookService.class.getDeclaredField("sessionPlayer");
        field.setAccessible(true);
        return (ChapterPlayer) field.get(service);
    }

    private void chapterBook() {
        send("{\"command\":\"load\",\"id\":1,\"path\":\"/missing-test-book.m4b\",\"title\":\"Book\",\"author\":\"Author\",\"position\":75000,\"duration\":180000,\"chapters\":[{\"title\":\"First\",\"start\":0,\"end\":60000},{\"title\":\"Second\",\"start\":60000,\"end\":180000}]}");
    }

    @Test public void systemTimelineAndSeekingAreChapterRelativeWhilePersistenceKeepsFilePositions() throws Exception {
        chapterBook();
        ChapterPlayer system = sessionPlayer();
        assertEquals(120000, system.getDuration());
        assertEquals(15000, system.getCurrentPosition());
        assertEquals("Book", system.getMediaMetadata().title.toString());
        assertEquals("Author · Chapter 2 · Second", system.getMediaMetadata().artist.toString());
        assertEquals("Chapter 2 · Second", system.getMediaMetadata().description.toString());
        system.seekTo(30000);
        shadowOf(Looper.getMainLooper()).idle();
        assertEquals(90000, player.getCurrentPosition());
        assertEquals(30000, system.getCurrentPosition());
        assertEquals(90000, events.get(events.size() - 1).getLong("position"));
        system.previousChapter(); // More than three seconds in: restart this chapter.
        assertEquals(60000, player.getCurrentPosition());
        system.previousChapter();
        assertEquals(0, player.getCurrentPosition());
        system.refresh();
        assertEquals(60000, system.getDuration());
        system.nextChapter();
        assertEquals(60000, player.getCurrentPosition());
    }

    @Test public void seekingToTheEndOfAChapterShowsTheNextChaptersTimeline() throws Exception {
        chapterBook();
        ChapterPlayer system = sessionPlayer();
        system.previousChapter();
        system.previousChapter();
        system.refresh();
        system.seekTo(60000);
        shadowOf(Looper.getMainLooper()).idle();
        system.refresh();
        assertEquals(60000, player.getCurrentPosition());
        assertEquals(0, system.getCurrentPosition());
        assertEquals(120000, system.getDuration());
        assertTrue(system.getMediaMetadata().description.toString().contains("Chapter 2"));
    }

    @Test public void artworkUpdatesRetainTheCurrentChapterPositionAndPlaybackState() throws Exception {
        chapterBook();
        player.pause();
        send("{\"command\":\"artwork\",\"id\":1,\"cover\":[1,2,3]}");
        ChapterPlayer system = sessionPlayer();
        assertArrayEquals(new byte[] {1,2,3}, system.getMediaMetadata().artworkData);
        assertEquals(75000, player.getCurrentPosition());
        assertEquals(15000, system.getCurrentPosition());
        assertFalse(player.getPlayWhenReady());
    }

    @Test public void timedSkipsCrossChapterBoundariesAndKeepTheirFullIncrement() throws Exception {
        chapterBook();
        ChapterPlayer system = sessionPlayer();
        player.seekTo(55000);
        system.skip(30000);
        assertEquals(85000, player.getCurrentPosition());
        assertEquals(25000, system.getCurrentPosition());
        system.skip(-15000);
        system.skip(-15000);
        assertEquals(55000, player.getCurrentPosition());
        system.refresh();
        assertEquals(60000, system.getDuration());
    }

    @Test public void chapterSeekOutsideCurrentFileRequestsTheOwningRustSession() throws Exception {
        send("{\"command\":\"load\",\"id\":1,\"path\":\"/missing-test-book.m4b\",\"title\":\"Book\",\"position\":10000,\"offset\":60000,\"duration\":120000,\"chapters\":[{\"title\":\"First\",\"start\":0,\"end\":60000},{\"title\":\"Second\",\"start\":60000,\"end\":180000}]}");
        ChapterPlayer system = sessionPlayer();
        system.previousChapter();
        system.previousChapter();
        assertEquals(0, events.get(events.size() - 1).getLong("seekTarget"));
    }

    private void shortFirstFile() {
        send("{\"command\":\"load\",\"id\":1,\"path\":\"/missing-test-book.m4b\",\"title\":\"Book\",\"duration\":60000,\"position\":55000,\"chapters\":[{\"title\":\"One\",\"start\":0,\"end\":60000},{\"title\":\"Two\",\"start\":60000,\"end\":120000},{\"title\":\"Three\",\"start\":120000,\"end\":180000}]}");
    }

    @Test public void rapidCrossFileSkipsAccumulateFromThePendingDestination() throws Exception {
        shortFirstFile();
        ChapterPlayer system = sessionPlayer();
        system.skip(30000);
        system.skip(30000);
        assertEquals(115000, events.get(events.size() - 1).getLong("seekTarget"));
        system.skip(-15000);
        assertEquals(100000, events.get(events.size() - 1).getLong("seekTarget"));
        system.nextChapter();
        assertEquals(120000, events.get(events.size() - 1).getLong("seekTarget"));
    }

    @Test public void reversingAPendingCrossFileSeekReplacesTheQueuedDestination() throws Exception {
        shortFirstFile();
        ChapterPlayer system = sessionPlayer();
        system.skip(30000);
        system.skip(-30000);
        assertEquals(55000, events.get(events.size() - 1).getLong("seekTarget"));
        // Rust acknowledges the target; later skips resume from the decoder.
        send("{\"command\":\"seek\",\"id\":1,\"sequence\":1,\"position\":55000}");
        system.skip(-15000);
        assertEquals(40000, player.getCurrentPosition());
    }

    @Test public void crossFileHandoffKeepsTheServiceAliveForAPausedReplacement() {
        shortFirstFile();
        send("{\"command\":\"detach\",\"id\":1}");
        assertFalse(shadowOf(service).isStoppedBySelf());
        assertNull(player.getCurrentMediaItem());
        send("{\"command\":\"load\",\"id\":2,\"path\":\"/missing-second-file.m4b\",\"title\":\"Book\",\"position\":15000,\"playWhenReady\":false}");
        assertEquals("2", player.getCurrentMediaItem().mediaId);
        assertFalse(player.getPlayWhenReady());
        assertEquals(15000, player.getCurrentPosition());
    }

    @Test public void cancellingAnIdleHandoffStopsTheServiceWithoutStoppingANewerBook() {
        shortFirstFile();
        send("{\"command\":\"detach\",\"id\":1}");
        send("{\"command\":\"stop\",\"id\":0}");
        assertTrue(shadowOf(service).isStoppedBySelf());
        send("{\"command\":\"load\",\"id\":2,\"path\":\"/missing-second-file.m4b\",\"title\":\"Book\",\"position\":15000,\"playWhenReady\":false}");
        service.onStartCommand(null, 0, 2);
        send("{\"command\":\"stop\",\"id\":0}");
        assertEquals("2", player.getCurrentMediaItem().mediaId);
    }

    @Test public void replacementQueuedWhileTheOldServiceStopsIsLoadedPausedOnRestart() throws Exception {
        shortFirstFile();
        send("{\"command\":\"stop\",\"id\":1}");
        send("{\"command\":\"load\",\"id\":2,\"path\":\"/missing-second-file.m4b\",\"title\":\"Book\",\"position\":15000,\"playWhenReady\":false}");
        service.onStartCommand(null, 0, 2);
        assertEquals("2", player.getCurrentMediaItem().mediaId);
        assertFalse(player.getPlayWhenReady());
        assertEquals(15000, player.getCurrentPosition());
    }

    @Test public void replacementFileCanBePreparedPausedAndResumedFromSystemControls() throws Exception {
        shortFirstFile();
        player.pause();
        send("{\"command\":\"load\",\"id\":2,\"path\":\"/missing-second-file.m4b\",\"title\":\"Book\",\"offset\":60000,\"duration\":120000,\"position\":15000,\"playWhenReady\":false,\"chapters\":[{\"title\":\"One\",\"start\":0,\"end\":60000},{\"title\":\"Two\",\"start\":60000,\"end\":180000}]}");
        assertFalse(player.getPlayWhenReady());
        assertEquals(15000, player.getCurrentPosition());
        assertNotNull(sessionPlayer().getCurrentMediaItem());
        assertTrue(sessionPlayer().getMediaMetadata().description.toString().contains("Chapter 2"));
        sessionPlayer().play();
        assertTrue(player.getPlayWhenReady());
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
    @Test public void oldReaderCannotSeekOrStopReplacementBook() {
        load(1, 10_000);
        load(2, 20_000);
        send("{\"command\":\"seek\",\"id\":1,\"position\":0,\"sequence\":1}");
        send("{\"command\":\"stop\",\"id\":1}");
        assertEquals("2", player.getCurrentMediaItem().mediaId);
        assertEquals(20_000, player.getCurrentPosition());
        assertTrue(events.stream().anyMatch(e -> e.optLong("id") == 1 && e.optBoolean("released")));
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
}
