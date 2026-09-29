package se.bokheim.reader.gpui;

/** Headless package probe: no Activity, backend, or user database is opened. */
final class UpdateMetadata {
    private UpdateMetadata() {}

    static String json() {
        System.loadLibrary("desktop_gpui");
        String result = nativeJson();
        if (result == null) throw new IllegalStateException("Native update metadata is unavailable");
        return result;
    }

    private static native String nativeJson();
}
