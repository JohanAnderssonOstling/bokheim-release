package se.bokheim.reader.gpui;

import androidx.media3.common.C;
import androidx.media3.common.ForwardingSimpleBasePlayer;
import androidx.media3.common.MediaItem;
import androidx.media3.common.MediaMetadata;
import androidx.media3.common.Player;
import com.google.common.util.concurrent.Futures;
import com.google.common.util.concurrent.ListenableFuture;
import java.util.ArrayList;
import java.util.List;
import java.util.function.LongConsumer;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

/** The system sees chapters; the decoder and Rust persistence retain file positions. */
@androidx.annotation.OptIn(markerClass = androidx.media3.common.util.UnstableApi.class)
final class ChapterPlayer extends ForwardingSimpleBasePlayer {
    private static final class Chapter {
        final long start, end;
        final String label;
        Chapter(JSONObject data, int index) throws JSONException {
            start = data.getLong("start");
            end = data.getLong("end");
            label = "Chapter " + (index + 1) + " · " + data.optString("title");
        }
    }
    private final LongConsumer remoteSeek;
    private final List<Chapter> chapters = new ArrayList<>();
    private long offset, fileDuration;
    private Long pendingPosition;
    private MediaMetadata metadata = MediaMetadata.EMPTY;
    private String bookId = "";
    private List<MediaItemData> items = java.util.Collections.emptyList();

    ChapterPlayer(Player player, LongConsumer remoteSeek) { super(player); this.remoteSeek = remoteSeek; }

    void configure(JSONObject command, MediaMetadata metadata) throws JSONException {
        pendingPosition = null;
        chapters.clear();
        JSONArray entries = command.optJSONArray("chapters");
        long previousEnd = 0;
        for (int i = 0; entries != null && i < entries.length(); i++) {
            Chapter chapter = new Chapter(entries.getJSONObject(i), i);
            if (chapter.start < previousEnd || chapter.end <= chapter.start) { chapters.clear(); break; }
            chapters.add(chapter);
            previousEnd = chapter.end;
        }
        offset = command.optLong("offset");
        fileDuration = command.optLong("duration", Long.MAX_VALUE - offset);
        this.metadata = metadata;
        bookId = Long.toString(command.getLong("id"));
        rebuildPlaylist();
    }

    private long globalPosition() { return pendingPosition == null ? offset + getPlayer().getCurrentPosition() : pendingPosition; }

    void clearPendingSeek() { pendingPosition = null; }

    int chapterIndex() {
        long position = globalPosition();
        for (int i = 0; i < chapters.size(); i++) if (position < chapters.get(i).end) return i;
        return chapters.size() - 1;
    }

    void setArtwork(MediaMetadata metadata) { this.metadata = metadata; rebuildPlaylist(); invalidateState(); }

    void refresh() { invalidateState(); }

    void seekGlobal(long position) {
        long end = chapters.isEmpty() ? offset + fileDuration : chapters.get(chapters.size() - 1).end;
        position = Math.max(0, Math.min(end, position));
        if (pendingPosition == null && position >= offset && (position < offset + fileDuration || position == end && position == offset + fileDuration)) getPlayer().seekTo(position - offset);
        else {
            // Until Rust opens/acknowledges the destination, each press builds
            // on the last requested position, including a return to this file.
            pendingPosition = position;
            remoteSeek.accept(position);
            invalidateState();
        }
    }

    void skip(long delta) { seekGlobal(globalPosition() + delta); }

    void previousChapter() {
        int index = chapterIndex();
        if (index < 0) return;
        if (globalPosition() - chapters.get(index).start <= 3000) index = Math.max(0, index - 1);
        seekGlobal(chapters.get(index).start);
    }

    void nextChapter() {
        int index = chapterIndex();
        if (index >= 0 && index + 1 < chapters.size()) seekGlobal(chapters.get(index + 1).start);
    }

    private void rebuildPlaylist() {
        items = new ArrayList<>();
        for (int i = 0; i < chapters.size(); i++) {
            Chapter chapter = chapters.get(i);
            String author = metadata.artist == null ? "" : metadata.artist.toString();
            MediaMetadata chapterMetadata = metadata.buildUpon().setArtist(MediaArtwork.credit(author, chapter.label))
                .setSubtitle(author).setDescription(chapter.label).setAlbumTitle(chapter.label).build();
            MediaItem item = new MediaItem.Builder().setMediaId(bookId + ":chapter:" + i).setMediaMetadata(chapterMetadata).build();
            items.add(new MediaItemData.Builder(item.mediaId).setMediaItem(item).setMediaMetadata(chapterMetadata)
                .setIsSeekable(true).setDurationUs((chapter.end - chapter.start) * 1000).build());
        }
    }

    @Override protected State getState() {
        State state = super.getState();
        if (chapters.isEmpty() || state.timeline.isEmpty()) return state;
        int index = chapterIndex();
        Chapter current = chapters.get(index);
        long length = current.end - current.start;
        State.Builder result = state.buildUpon().setPlaylist(items).setCurrentMediaItemIndex(index)
            .setAvailableCommands(state.availableCommands.buildUpon().add(Player.COMMAND_SEEK_IN_CURRENT_MEDIA_ITEM).build())
            .setContentPositionMs(() -> Math.max(0, Math.min(length, globalPosition() - current.start)))
            .setContentBufferedPositionMs(() -> Math.max(0, Math.min(length, offset + getPlayer().getBufferedPosition() - current.start)));
        if (state.hasPositionDiscontinuity) result.setPositionDiscontinuity(state.positionDiscontinuityReason,
            Math.max(0, Math.min(length, offset + state.discontinuityPositionMs - current.start)));
        return result.build();
    }

    @Override protected ListenableFuture<?> handleSeek(int index, long positionMs, int command) {
        if (chapters.isEmpty()) return super.handleSeek(index, positionMs, command);
        if (command == Player.COMMAND_SEEK_BACK) skip(-15_000);
        else if (command == Player.COMMAND_SEEK_FORWARD) skip(30_000);
        else if (command == Player.COMMAND_SEEK_TO_PREVIOUS || command == Player.COMMAND_SEEK_TO_PREVIOUS_MEDIA_ITEM) previousChapter();
        else if (command == Player.COMMAND_SEEK_TO_NEXT || command == Player.COMMAND_SEEK_TO_NEXT_MEDIA_ITEM) nextChapter();
        else if (index >= 0 && index < chapters.size()) {
            Chapter chapter = chapters.get(index);
            seekGlobal(chapter.start + (positionMs == C.TIME_UNSET ? 0 : Math.max(0, Math.min(chapter.end - chapter.start, positionMs))));
        }
        return Futures.immediateVoidFuture();
    }
}
