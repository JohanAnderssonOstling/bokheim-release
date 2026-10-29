package se.bokheim.reader.gpui;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ServiceInfo;
import android.media.AudioAttributes;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
import android.media.MediaMetadata;
import android.media.session.MediaSession;
import android.media.session.PlaybackState;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.PowerManager;
import android.util.Log;
import android.view.KeyEvent;
import org.json.JSONObject;

/** Android hosts the media session; Rust owns synthesis and its continuous PCM queue. */
public final class SpeechService extends Service {
    static final int NOTIFICATION_ID = 1042;
    private static final String CHANNEL = "bokheim-read-aloud";
    private static final Handler MAIN = new Handler(Looper.getMainLooper());
    private static final java.util.ArrayDeque<Long> pending = new java.util.ArrayDeque<>();
    private static SpeechService instance;
    private static native String nativeState(long id);
    private static native boolean nativeControl(long id, String command);
    static java.util.function.LongFunction<String> stateProvider = SpeechService::nativeState;
    static java.util.function.BiPredicate<Long, String> controlSender = SpeechService::nativeControl;
    private MediaSession session;
    private MediaSession.Callback controls;
    private PowerManager.WakeLock wakeLock;
    private AudioManager audio;
    private AudioFocusRequest focus;
    private boolean hasFocus;
    private boolean foreground;
    private long id;
    private String lastState;
    private String author = "", chapter = "";
    private byte[] coverBytes;
    private android.graphics.Bitmap cover;
    private final Runnable deferredStop = this::stopSelf;
    private final BroadcastReceiver noisy = new BroadcastReceiver() {
        @Override public void onReceive(Context context, Intent intent) {
            if (AudioManager.ACTION_AUDIO_BECOMING_NOISY.equals(intent.getAction())) command("pause");
        }
    };

    // Only the in-process bridge can start narration. Notification intents can
    // control an existing session but cannot supply a book or voice/file path.
    public static void dispatch(Context context, String encoded) {
        MAIN.post(() -> {
            try {
                JSONObject data = new JSONObject(encoded);
                long id = data.getLong("id");
                String command = data.getString("command");
                if ("start".equals(command)) {
                    if (instance != null) instance.begin(id);
                    else {
                        boolean start = pending.isEmpty();
                        pending.addLast(id);
                        try { if (start) context.startForegroundService(new Intent(context, SpeechService.class)); }
                        catch (RuntimeException error) { pending.remove(id); throw error; }
                    }
                } else if (instance != null && instance.id == id) {
                    if ("update".equals(command)) instance.updateState();
                    else if ("stop".equals(command)) instance.finishSession();
                }
            } catch (Exception error) {
                Log.e("Bokheim", "Could not start speech media service", error);
                try { controlSender.test(new JSONObject(encoded).getLong("id"), "stop"); }
                catch (Exception ignored) { }
            }
        });
    }

    @Override public void onCreate() {
        super.onCreate();
        instance = this;
        NotificationChannel channel = new NotificationChannel(CHANNEL, "Read aloud", NotificationManager.IMPORTANCE_LOW);
        channel.setSound(null, null);
        channel.setLockscreenVisibility(Notification.VISIBILITY_PUBLIC);
        getSystemService(NotificationManager.class).createNotificationChannel(channel);
        wakeLock = getSystemService(PowerManager.class).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "Bokheim:ReadAloud");
        wakeLock.setReferenceCounted(false);
        audio = getSystemService(AudioManager.class);
        focus = new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
            .setAudioAttributes(new AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA).setContentType(AudioAttributes.CONTENT_TYPE_SPEECH).build())
            .setOnAudioFocusChangeListener(this::focusChanged, MAIN).build();
        session = new MediaSession(this, "BokheimReadAloud");
        controls = new MediaSession.Callback() {
            @Override public boolean onMediaButtonEvent(Intent intent) {
                KeyEvent event = intent.getParcelableExtra(Intent.EXTRA_KEY_EVENT);
                if (event != null && (event.getKeyCode() == KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE || event.getKeyCode() == KeyEvent.KEYCODE_HEADSETHOOK)) {
                    if (event.getAction() == KeyEvent.ACTION_DOWN && event.getRepeatCount() == 0) {
                        try {
                            boolean paused = new JSONObject(stateProvider.apply(id)).getBoolean("paused");
                            command(paused ? "resume" : "pause");
                        } catch (Exception ignored) { }
                    }
                    return true;
                }
                return super.onMediaButtonEvent(intent);
            }
            @Override public void onPlay() { command("resume"); }
            @Override public void onPause() { command("pause"); }
            @Override public void onStop() { command("stop"); }
            @Override public void onRewind() { command("back"); }
            @Override public void onFastForward() { command("forward"); }
            @Override public void onSkipToPrevious() { command("back"); }
            @Override public void onSkipToNext() { command("forward"); }
            @Override public void onCustomAction(String action, Bundle extras) {
                if ("stop".equals(action)) command("stop");
            }
        };
        session.setCallback(controls, MAIN);
        session.setSessionActivity(openReader());
        if (Build.VERSION.SDK_INT >= 33) registerReceiver(noisy, new IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY), Context.RECEIVER_NOT_EXPORTED);
        else registerReceiver(noisy, new IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY));
    }

    @Override public int onStartCommand(Intent intent, int flags, int startId) {
        while (!pending.isEmpty()) begin(pending.removeFirst());
        if (intent != null && id != 0 && intent.getLongExtra("id", 0) == id && intent.getAction() != null) command(intent.getAction());
        // A cold notification tap never restores or starts a stale narration.
        if (id == 0) stopSelf();
        return START_NOT_STICKY;
    }

    void begin(long next) {
        MAIN.removeCallbacks(deferredStop);
        if (id != 0 && id != next) controlSender.test(id, "stop");
        id = next;
        lastState = null;
        author = chapter = "";
        coverBytes = null;
        cover = null;
        session.setActive(true);
        // Promote before requesting focus (required when targeting Android 15+).
        promote(notification("Read aloud", false));
        updateState();
    }

    private void promote(Notification notification) {
        lastState = null;
        if (Build.VERSION.SDK_INT >= 29) startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);
        else startForeground(NOTIFICATION_ID, notification);
        foreground = true;
    }

    void command(String action) {
        if (id == 0) return;
        if ("resume".equals(action)) {
            promote(notification(title(), false));
            if (!requestFocus()) return;
        }
        if (controlSender.test(id, action)) {
            if ("stop".equals(action)) finishSession();
            else updateState();
        }
    }

    void focusChanged(int change) {
        if (change < 0) {
            audio.abandonAudioFocusRequest(focus);
            hasFocus = false;
            command("pause");
        }
    }

    private boolean requestFocus() {
        if (hasFocus) return true;
        hasFocus = audio.requestAudioFocus(focus) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED;
        if (!hasFocus && controlSender.test(id, "pause")) MAIN.post(this::updateState);
        return hasFocus;
    }

    private String title() {
        try { return new JSONObject(stateProvider.apply(id)).optString("title", "Read aloud"); }
        catch (Exception ignored) { return "Read aloud"; }
    }

    void updateState() {
        if (id == 0) return;
        try {
            String encoded = stateProvider.apply(id);
            if (encoded == null || encoded.isEmpty()) { finishSession(); return; }
            JSONObject state = new JSONObject(encoded);
            boolean paused = state.getBoolean("paused");
            String title = state.optString("title", "Read aloud");
            if (!paused && !hasFocus) {
                promote(notification(title, false));
                if (!requestFocus()) return;
            }
            if (paused) {
                if (wakeLock.isHeld()) wakeLock.release();
                if (hasFocus) { audio.abandonAudioFocusRequest(focus); hasFocus = false; }
                if (foreground) { stopForeground(false); foreground = false; }
            } else if (!wakeLock.isHeld()) wakeLock.acquire();
            if (encoded.equals(lastState)) return;
            lastState = encoded;
            author = state.optString("author");
            chapter = state.optString("chapter");
            byte[] bytes = MediaArtwork.bytes(state);
            if (!java.util.Arrays.equals(bytes, coverBytes)) {
                coverBytes = bytes;
                cover = MediaArtwork.bitmap(bytes);
            }
            session.setMetadata(new MediaMetadata.Builder().putString(MediaMetadata.METADATA_KEY_TITLE, title)
                .putString(MediaMetadata.METADATA_KEY_ARTIST, MediaArtwork.credit(author, chapter))
                .putString(MediaMetadata.METADATA_KEY_DISPLAY_TITLE, title)
                .putString(MediaMetadata.METADATA_KEY_DISPLAY_SUBTITLE, author)
                .putString(MediaMetadata.METADATA_KEY_DISPLAY_DESCRIPTION, chapter)
                .putBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART, cover).build());
            int playback = paused ? PlaybackState.STATE_PAUSED : state.optBoolean("preparing") ? PlaybackState.STATE_BUFFERING : PlaybackState.STATE_PLAYING;
            session.setPlaybackState(new PlaybackState.Builder().setActions(PlaybackState.ACTION_PLAY | PlaybackState.ACTION_PAUSE
                | PlaybackState.ACTION_PLAY_PAUSE | PlaybackState.ACTION_STOP | PlaybackState.ACTION_REWIND | PlaybackState.ACTION_FAST_FORWARD
                | PlaybackState.ACTION_SKIP_TO_PREVIOUS | PlaybackState.ACTION_SKIP_TO_NEXT)
                .setState(playback, PlaybackState.PLAYBACK_POSITION_UNKNOWN, (float) state.optDouble("rate", 1.0)).build());
            getSystemService(NotificationManager.class).notify(NOTIFICATION_ID, notification(title, paused));
        } catch (Exception error) {
            Log.e("Bokheim", "Could not update speech media session", error);
            controlSender.test(id, "stop");
            finishSession();
        }
    }

    private PendingIntent openReader() {
        return PendingIntent.getActivity(this, 1042, new Intent(this, MainActivity.class).setFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP), PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
    }

    private Notification.Action action(int icon, String title, String command, int request) {
        Intent intent = new Intent(this, SpeechService.class).setAction(command).putExtra("id", id);
        return new Notification.Action.Builder(icon, title, PendingIntent.getService(this, request, intent, PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE)).build();
    }

    private Notification notification(String title, boolean paused) {
        return new Notification.Builder(this, CHANNEL).setSmallIcon(android.R.drawable.ic_media_play)
            .setContentTitle(title).setContentText(author).setSubText(chapter).setLargeIcon(cover).setContentIntent(openReader())
            .setVisibility(Notification.VISIBILITY_PUBLIC).setOnlyAlertOnce(true).setOngoing(!paused)
            .addAction(action(android.R.drawable.ic_media_rew, "Previous passage", "back", 1043))
            .addAction(action(paused ? android.R.drawable.ic_media_play : android.R.drawable.ic_media_pause, paused ? "Play" : "Pause", paused ? "resume" : "pause", 1044))
            .addAction(action(android.R.drawable.ic_media_ff, "Next passage", "forward", 1045))
            .setStyle(new Notification.MediaStyle().setMediaSession(session.getSessionToken()).setShowActionsInCompactView(0, 1, 2)).build();
    }

    private void finishSession() {
        if (wakeLock.isHeld()) wakeLock.release();
        audio.abandonAudioFocusRequest(focus);
        hasFocus = false;
        session.setActive(false);
        session.setMetadata(null);
        session.setPlaybackState(new PlaybackState.Builder().setState(PlaybackState.STATE_STOPPED, PlaybackState.PLAYBACK_POSITION_UNKNOWN, 0).build());
        coverBytes = null;
        cover = null;
        stopForeground(true);
        foreground = false;
        getSystemService(NotificationManager.class).cancel(NOTIFICATION_ID);
        id = 0;
        lastState = null;
        // A quick new chapter/book start can reuse this service without racing
        // onDestroy from an earlier stop request.
        MAIN.removeCallbacks(deferredStop);
        MAIN.postDelayed(deferredStop, 250);
    }

    @Override public void onDestroy() {
        MAIN.removeCallbacks(deferredStop);
        if (id != 0) controlSender.test(id, "stop");
        if (wakeLock.isHeld()) wakeLock.release();
        audio.abandonAudioFocusRequest(focus);
        unregisterReceiver(noisy);
        session.setActive(false);
        session.release();
        if (instance == this) instance = null;
        super.onDestroy();
    }

    @Override public IBinder onBind(Intent intent) { return null; }
}
