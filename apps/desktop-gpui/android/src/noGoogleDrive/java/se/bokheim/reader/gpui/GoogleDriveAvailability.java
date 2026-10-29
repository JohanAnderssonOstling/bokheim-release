package se.bokheim.reader.gpui;

import android.content.Context;

/** Release builds have no Drive integration or Play services authorization SDK. */
final class GoogleDriveAvailability {
    private GoogleDriveAvailability() {}

    static boolean isAvailable(Context context) { return false; }
}
