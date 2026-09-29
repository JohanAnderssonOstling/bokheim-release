package se.bokheim.reader.gpui;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ClipData;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.os.Environment;
import android.os.ParcelFileDescriptor;
import android.provider.DocumentsContract;
import android.provider.OpenableColumns;
import android.util.Log;
import android.view.KeyEvent;
import android.view.WindowManager;

import androidx.activity.result.ActivityResultLauncher;
import androidx.activity.result.contract.ActivityResultContracts;
import androidx.core.graphics.Insets;
import androidx.core.view.ViewCompat;
import androidx.core.view.WindowCompat;
import androidx.core.view.WindowInsetsAnimationCompat;
import androidx.core.view.WindowInsetsCompat;
import androidx.core.view.WindowInsetsControllerCompat;

import com.google.androidgamesdk.GameActivity;

import org.json.JSONArray;

import java.io.IOException;
import java.io.File;
import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Locale;
import java.util.Set;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

public final class MainActivity extends GameActivity {
    private static volatile java.lang.ref.WeakReference<MainActivity> accountActivity = new java.lang.ref.WeakReference<>(null);
    private AccountDialog accountDialog;
    private native void nativeAuthenticationRequest(long id, String message);

    // JNI calls from the GPUI thread; all Android views stay on the main thread.
    public static void authenticationCommand(String encoded) {
        MainActivity activity = accountActivity.get();
        if (activity == null) return;
        activity.runOnUiThread(() -> activity.applyAuthenticationCommand(encoded));
    }
    private void applyAuthenticationCommand(String encoded) {
        if (isFinishing() || isDestroyed()) return;
        try {
            org.json.JSONObject message = new org.json.JSONObject(encoded);
            long id = message.getLong("id");
            switch (message.getString("command")) {
                case "open":
                    if (accountDialog != null) accountDialog.dismiss();
                    org.json.JSONArray palette = message.getJSONArray("colors");
                    int[] colors = new int[5];
                    for (int i = 0; i < colors.length; i++) colors[i] = (int) palette.getLong(i);
                    accountDialog = new AccountDialog(this, id, message.getInt("minimum"), colors, (session, request) -> {
                        nativeAuthenticationRequest(session, request);
                        try {
                            if (new org.json.JSONObject(request).getString("action").equals("closed") && accountDialog != null && accountDialog.sessionId == session) accountDialog = null;
                        } catch (org.json.JSONException ignored) { }
                    });
                    accountDialog.show();
                    break;
                case "result":
                    if (accountDialog != null && accountDialog.sessionId == id) accountDialog.complete(message.isNull("error") ? null : message.getString("error"));
                    break;
                case "close":
                    if (accountDialog != null && accountDialog.sessionId == id) accountDialog.dismiss();
                    break;
            }
        } catch (org.json.JSONException malformed) {
            // Do not log the payload: account messages may contain credentials.
            android.util.Log.e(LOG_TAG, "Invalid authentication command");
        }
    }

    private static final String LOG_TAG = "Bokheim";
    private static final String DIRECTORY_MIME = DocumentsContract.Document.MIME_TYPE_DIR;
    private static final String[] BOOK_EXTENSIONS = {"epub", "pdf", "mobi", "azw", "azw3", "m4b"};

    private final ExecutorService fileExecutor = Executors.newSingleThreadExecutor();
    private ActivityResultLauncher<Intent> pathPromptLauncher;
    private ActivityResultLauncher<Intent> updateInstallerLauncher;

    /** Called on the native update worker. Every UI command originates from an
     * explicit Settings action; installer return codes never prove activation.
     */
    public static String updateCommand(android.content.Context context, String encoded) throws Exception {
        org.json.JSONObject request = new org.json.JSONObject(encoded);
        String action = request.getString("action");
        if (action.equals("prepare")) {
            UpdateMigrationService.prepare(context, request.getString("path"));
            return "";
        }
        if (action.equals("permission")) return Boolean.toString(ApkInstaller.permissionGranted(context));
        MainActivity activity = accountActivity.get();
        if (activity == null || activity.isFinishing() || activity.isDestroyed()) throw new IllegalStateException("No active update window");
        if (action.equals("install")) {
            if (!ApkInstaller.permissionGranted(context)) return "permission";
            final File apk;
            try {
                apk = ApkInstaller.verify(context, new File(request.getString("path")), request.getString("version"), request.getLong("bytes"), request.getString("sha256"));
            } catch (ApkInstaller.InvalidPackageException invalid) {
                Log.w(LOG_TAG, "Quarantining invalid update package", invalid);
                return "invalidPackage";
            }
            activity.runUpdateAction(() -> activity.updateInstallerLauncher.launch(ApkInstaller.installIntent(activity, apk)));
        } else if (action.equals("grantPermission")) {
            activity.runUpdateAction(() -> activity.startActivity(ApkInstaller.permissionIntent(activity)));
        } else if (action.equals("restart")) {
            activity.runUpdateAction(() -> activity.startActivity(new Intent(activity, UpdateRestartActivity.class)
                .putExtra("old_pid", android.os.Process.myPid()).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)));
        } else throw new IllegalArgumentException("Unknown update action");
        return "";
    }
    private void runUpdateAction(Runnable action) {
        runOnUiThread(() -> {
            if (isFinishing() || isDestroyed()) return;
            try { action.run(); }
            catch (RuntimeException error) {
                // The approved package remains available; an unavailable Android
                // installer/settings activity must not crash the running reader.
                Log.w(LOG_TAG, "Could not open Android update action", error);
            }
        });
    }
    private PathPrompt pendingPathPrompt;
    private volatile boolean captureVolumeButtons;
    private boolean systemBarsVisible = true;
    private boolean immersiveBarsHidden;
    private long suppressReaderChromeRevealUntil;
    private boolean keepScreenAwake;
    private Insets lastSafeAreaInsets;
    private Insets lastImeInsets;
    private AlertDialog storagePermissionPrompt;
    private Boolean publishedStorageAccess;

    private static final class PathPrompt {
        final long id;
        final boolean returnFileDescriptors;
        final boolean returnDirectoryTree;
        final String[] extensions;

        PathPrompt(long id, boolean returnFileDescriptors, boolean returnDirectoryTree, String[] extensions) {
            this.id = id;
            this.returnFileDescriptors = returnFileDescriptors;
            this.returnDirectoryTree = returnDirectoryTree;
            this.extensions = extensions;
        }
    }

    private static final class DocumentInfo {
        final String id;
        final String name;
        final String mimeType;

        DocumentInfo(String id, String name, String mimeType) {
            this.id = id;
            this.name = name;
            this.mimeType = mimeType;
        }
    }

    private native void nativeSelectedFileDescriptor(long requestId, String name, int fileDescriptor);
    private native void nativeFilePromptResult(long requestId, String error, boolean cancelled);
    private native void nativeSelectedDirectoryRoot(long requestId, String name);
    private native void nativeSelectedDirectoryPath(long requestId, String pathJson);
    private native void nativeSelectedDirectoryFile(long requestId, String pathJson, String uri, long sizeBytes);
    private native void nativeDirectoryPromptResult(long requestId, String error, boolean cancelled);
    private native void nativeSelectedDirectoryLocalRoot(long requestId, String path);
    private native void nativeStorageAccessChanged(boolean granted, String documentsLibraryRoot);
    private native void nativeOpenBook(String name, int fileDescriptor);
    private native void nativeVolumeButton(boolean next);
    private native void nativeWindowInsets(
        int safeLeft,
        int safeTop,
        int safeRight,
        int safeBottom,
        int imeLeft,
        int imeTop,
        int imeRight,
        int imeBottom
    );
    private native void nativeReaderChromeRevealed();

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        accountActivity = new java.lang.ref.WeakReference<>(this);
        updateInstallerLauncher = registerForActivityResult(
            new ActivityResultContracts.StartActivityForResult(), result -> {
                // Keep the approved package on cancellation/failure. The next
                // native poll reads permission; next launch reads installed code.
            });
        pathPromptLauncher = registerForActivityResult(
            new ActivityResultContracts.StartActivityForResult(),
            result -> handlePathPromptResult(result.getResultCode(), result.getData())
        );
        WindowCompat.setDecorFitsSystemWindows(getWindow(), false);
        installWindowInsetsReporting();
        applySystemBarsVisibility();
        // Opening a shared book should go straight to the reader. Offer the
        // optional storage permission on an ordinary app launch instead.
        promptForStorageAccess(getIntent());
        handleIncomingBookIntent(getIntent());
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        promptForStorageAccess(intent);
        handleIncomingBookIntent(intent);
    }

    private void promptForStorageAccess(Intent intent) {
        if (storagePermissionPrompt == null || !storagePermissionPrompt.isShowing()) {
            storagePermissionPrompt = AllFilesAccess.promptForLaunch(this, intent);
        }
    }

    public void requestAllFilesAccess() {
        AllFilesAccess.openSettings(this);
    }

    @Override
    protected void onResume() {
        super.onResume();
        applyKeepScreenAwake();
        boolean granted = AllFilesAccess.isGranted();
        if (publishedStorageAccess == null || publishedStorageAccess != granted) {
            publishedStorageAccess = granted;
            File documentsRoot = new File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOCUMENTS), "Bokheim");
            nativeStorageAccessChanged(granted, documentsRoot.getAbsolutePath());
        }
    }

    private void handleIncomingBookIntent(Intent intent) {
        if (intent == null || !Intent.ACTION_VIEW.equals(intent.getAction()) || intent.getData() == null) {
            return;
        }
        Uri uri = intent.getData();
        intent.setAction(null);
        intent.setData(null);
        fileExecutor.execute(() -> {
            try {
                String name = queryDisplayName(uri);
                if (!matchesExtension(name, BOOK_EXTENSIONS)) {
                    Log.w(LOG_TAG, "Ignoring unsupported incoming book " + name);
                    return;
                }
                try (ParcelFileDescriptor descriptor = getContentResolver().openFileDescriptor(uri, "r")) {
                    if (descriptor == null) {
                        throw new IOException("The document provider could not open " + name);
                    }
                    nativeOpenBook(name, descriptor.detachFd());
                }
            } catch (Exception error) {
                Log.e(LOG_TAG, "Could not open incoming book", error);
            }
        });
    }

    public void openFilePrompt(long requestId, boolean multiple, String extensionList) {
        if (pendingPathPrompt != null) {
            nativeFilePromptResult(requestId, "Another path prompt is already open", false);
            return;
        }

        String[] extensions = extensionList.isEmpty() ? new String[0] : extensionList.split("\\n");
        pendingPathPrompt = new PathPrompt(requestId, true, false, extensions);
        try {
            pathPromptLauncher.launch(filePromptIntent(multiple, extensions));
        } catch (RuntimeException error) {
            pendingPathPrompt = null;
            nativeFilePromptResult(requestId, error.toString(), false);
        }
    }

    public void openDirectoryPrompt(long requestId, String extensionList) {
        if (pendingPathPrompt != null) {
            nativeDirectoryPromptResult(requestId, "Another path prompt is already open", false);
            return;
        }
        String[] extensions = extensionList.isEmpty() ? new String[0] : extensionList.split("\\n");
        pendingPathPrompt = new PathPrompt(requestId, false, true, extensions);
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
        try {
            pathPromptLauncher.launch(intent);
        } catch (RuntimeException error) {
            pendingPathPrompt = null;
            nativeDirectoryPromptResult(requestId, error.toString(), false);
        }
    }

    public int openDocumentDescriptor(String uriText) {
        try {
            ParcelFileDescriptor descriptor = getContentResolver().openFileDescriptor(Uri.parse(uriText), "r");
            return descriptor == null ? -1 : descriptor.detachFd();
        } catch (Exception error) {
            return -1;
        }
    }

    public static void audiobookCommand(android.content.Context context, String command) {
        AudiobookService.dispatch(context, command);
    }

    public void setVolumeButtonCapture(boolean enabled) {
        captureVolumeButtons = enabled;
    }

    public void setSystemBarsVisible(boolean visible) {
        if (systemBarsVisible != visible) {
            // A visibility transition requested by the reader can itself
            // produce a system-bar animation. Do not interpret that animation
            // (or an immediate follow-up) as the user's edge-swipe reveal.
            suppressReaderChromeRevealUntil = android.os.SystemClock.uptimeMillis() + 400;
            immersiveBarsHidden = false;
        }
        systemBarsVisible = visible;
        applySystemBarsVisibility();
    }

    public void setKeepScreenAwake(boolean awake) {
        keepScreenAwake = awake;
        applyKeepScreenAwake();
    }

    @Override
    protected void onPause() {
        getWindow().clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        super.onPause();
    }

    @Override
    public boolean dispatchKeyEvent(KeyEvent event) {
        int keyCode = event.getKeyCode();
        boolean isVolumeButton = keyCode == KeyEvent.KEYCODE_VOLUME_UP || keyCode == KeyEvent.KEYCODE_VOLUME_DOWN;
        if (!captureVolumeButtons || !isVolumeButton) {
            return super.dispatchKeyEvent(event);
        }
        if (event.getAction() == KeyEvent.ACTION_DOWN && event.getRepeatCount() == 0) {
            nativeVolumeButton(keyCode == KeyEvent.KEYCODE_VOLUME_DOWN);
        }
        return true;
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) {
            applySystemBarsVisibility();
        }
    }

    private void handlePathPromptResult(int resultCode, Intent data) {
        if (pendingPathPrompt == null) {
            return;
        }

        PathPrompt prompt = pendingPathPrompt;
        pendingPathPrompt = null;
        if (resultCode != Activity.RESULT_OK || data == null) {
            if (prompt.returnDirectoryTree) {
                nativeDirectoryPromptResult(prompt.id, null, true);
            } else if (prompt.returnFileDescriptors) {
                nativeFilePromptResult(prompt.id, null, true);
            }
            return;
        }

        fileExecutor.execute(() -> {
            try {
                if (prompt.returnDirectoryTree) {
                    transferSelectedDirectory(prompt, data);
                    return;
                } else if (prompt.returnFileDescriptors) {
                    transferSelectedFileDescriptors(prompt, data);
                    return;
                }
            } catch (Exception error) {
                if (prompt.returnDirectoryTree) {
                    nativeDirectoryPromptResult(prompt.id, error.toString(), false);
                } else if (prompt.returnFileDescriptors) {
                    nativeFilePromptResult(prompt.id, error.toString(), false);
                }
            }
        });
    }

    @Override
    protected void onDestroy() {
        if (accountDialog != null) accountDialog.dismiss();
        if (accountActivity.get() == this) accountActivity.clear();
        fileExecutor.shutdownNow();
        super.onDestroy();
    }

    private void applySystemBarsVisibility() {
        WindowInsetsControllerCompat controller = WindowCompat.getInsetsController(getWindow(), getWindow().getDecorView());
        controller.setSystemBarsBehavior(WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
        if (systemBarsVisible) {
            controller.show(WindowInsetsCompat.Type.systemBars());
        } else {
            controller.hide(WindowInsetsCompat.Type.systemBars());
        }
    }

    private void installWindowInsetsReporting() {
        android.view.View decorView = getWindow().getDecorView();
        ViewCompat.setOnApplyWindowInsetsListener(decorView, (view, insets) -> {
            publishWindowInsets(insets);
            return insets;
        });
        ViewCompat.setWindowInsetsAnimationCallback(
            decorView,
            new WindowInsetsAnimationCompat.Callback(
                WindowInsetsAnimationCompat.Callback.DISPATCH_MODE_CONTINUE_ON_SUBTREE
            ) {
                @Override
                public WindowInsetsAnimationCompat.BoundsCompat onStart(
                    WindowInsetsAnimationCompat animation,
                    WindowInsetsAnimationCompat.BoundsCompat bounds
                ) {
                    // The second system-bar animation after immersive mode has
                    // settled is Android's own edge reveal. It is delivered
                    // even on devices where transient bars do not relayout.
                    if (!systemBarsVisible
                        && immersiveBarsHidden
                        && android.os.SystemClock.uptimeMillis() >= suppressReaderChromeRevealUntil
                        && (animation.getTypeMask() & WindowInsetsCompat.Type.systemBars()) != 0) {
                        immersiveBarsHidden = false;
                        nativeReaderChromeRevealed();
                    }
                    return bounds;
                }

                @Override
                public WindowInsetsCompat onProgress(
                    WindowInsetsCompat insets,
                    List<WindowInsetsAnimationCompat> runningAnimations
                ) {
                    publishWindowInsets(insets);
                    return insets;
                }
            }
        );
        ViewCompat.requestApplyInsets(decorView);
    }

    private void publishWindowInsets(WindowInsetsCompat insets) {
        if (!systemBarsVisible && !insets.isVisible(WindowInsetsCompat.Type.systemBars())) {
            immersiveBarsHidden = true;
        }
        Insets safeArea = insets.getInsets(
            WindowInsetsCompat.Type.systemBars() | WindowInsetsCompat.Type.displayCutout()
        );
        Insets ime = insets.getInsets(WindowInsetsCompat.Type.ime());
        if (safeArea.equals(lastSafeAreaInsets) && ime.equals(lastImeInsets)) {
            return;
        }
        lastSafeAreaInsets = safeArea;
        lastImeInsets = ime;
        nativeWindowInsets(
            safeArea.left,
            safeArea.top,
            safeArea.right,
            safeArea.bottom,
            ime.left,
            ime.top,
            ime.right,
            ime.bottom
        );
    }

    private void applyKeepScreenAwake() {
        if (keepScreenAwake) {
            getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        } else {
            getWindow().clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        }
    }

    private void transferSelectedFileDescriptors(PathPrompt prompt, Intent data) throws IOException {
        for (Uri uri : selectedFileUris(data)) {
            String name = queryDisplayName(uri);
            try (ParcelFileDescriptor descriptor = getContentResolver().openFileDescriptor(uri, "r")) {
                if (descriptor == null) {
                    throw new IOException("The document provider could not open " + name);
                }
                nativeSelectedFileDescriptor(prompt.id, name, descriptor.detachFd());
            }
        }
        nativeFilePromptResult(prompt.id, null, false);
    }

    private void transferSelectedDirectory(PathPrompt prompt, Intent data) throws IOException {
        Uri treeUri = data.getData();
        if (treeUri == null) {
            throw new IOException("The document provider returned no directory");
        }
        String rootId = DocumentsContract.getTreeDocumentId(treeUri);
        Uri rootUri = DocumentsContract.buildDocumentUriUsingTree(treeUri, rootId);
        DocumentInfo root = queryDocument(rootUri);
        nativeSelectedDirectoryRoot(prompt.id, root.name);
        File localRoot = AndroidLibraryStorage.localDirectory(this, treeUri);
        if (localRoot != null) {
            nativeSelectedDirectoryLocalRoot(prompt.id, localRoot.getAbsolutePath());
            nativeDirectoryPromptResult(prompt.id, null, false);
            return;
        }
        collectDirectoryTree(prompt, treeUri, root.id, new ArrayList<>(), new LinkedHashSet<>());
        nativeDirectoryPromptResult(prompt.id, null, false);
    }

    private void collectDirectoryTree(PathPrompt prompt, Uri treeUri, String parentDocumentId, List<String> parentPath, Set<String> visitedDirectories) throws IOException {
        if (!visitedDirectories.add(parentDocumentId)) {
            return;
        }
        Uri childrenUri = DocumentsContract.buildChildDocumentsUriUsingTree(treeUri, parentDocumentId);
        String[] projection = {
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_MIME_TYPE,
            DocumentsContract.Document.COLUMN_SIZE
        };
        try (Cursor cursor = getContentResolver().query(childrenUri, projection, null, null, null)) {
            if (cursor == null) {
                throw new IOException("The document provider could not list " + parentDocumentId);
            }
            while (cursor.moveToNext()) {
                DocumentInfo child = documentInfo(cursor);
                List<String> path = new ArrayList<>(parentPath);
                path.add(child.name);
                JSONArray pathJson = new JSONArray(path);
                if (DIRECTORY_MIME.equals(child.mimeType)) {
                    nativeSelectedDirectoryPath(prompt.id, pathJson.toString());
                    collectDirectoryTree(prompt, treeUri, child.id, path, visitedDirectories);
                } else if (matchesExtension(child.name, prompt.extensions)) {
                    Uri childUri = DocumentsContract.buildDocumentUriUsingTree(treeUri, child.id);
                    int sizeColumn = cursor.getColumnIndex(DocumentsContract.Document.COLUMN_SIZE);
                    long sizeBytes = sizeColumn >= 0 && !cursor.isNull(sizeColumn) ? cursor.getLong(sizeColumn) : -1;
                    nativeSelectedDirectoryFile(prompt.id, pathJson.toString(), childUri.toString(), sizeBytes);
                }
            }
        }
    }

    private static boolean matchesExtension(String name, String[] extensions) {
        if (extensions.length == 0) {
            return true;
        }
        int separator = name.lastIndexOf('.');
        if (separator < 0 || separator == name.length() - 1) {
            return false;
        }
        String extension = name.substring(separator + 1).toLowerCase(Locale.ROOT);
        for (String accepted : extensions) {
            if (extension.equals(accepted.toLowerCase(Locale.ROOT))) {
                return true;
            }
        }
        return false;
    }

    private static Set<Uri> selectedFileUris(Intent data) throws IOException {
        Set<Uri> uris = new LinkedHashSet<>();
        if (data.getData() != null) {
            uris.add(data.getData());
        }
        ClipData clipData = data.getClipData();
        if (clipData != null) {
            for (int index = 0; index < clipData.getItemCount(); index++) {
                uris.add(clipData.getItemAt(index).getUri());
            }
        }
        if (uris.isEmpty()) {
            throw new IOException("The document provider returned no files");
        }
        return uris;
    }

    private DocumentInfo queryDocument(Uri uri) throws IOException {
        String[] projection = {
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_MIME_TYPE
        };
        try (Cursor cursor = getContentResolver().query(uri, projection, null, null, null)) {
            if (cursor == null || !cursor.moveToFirst()) {
                throw new IOException("The document provider returned no metadata for " + uri);
            }
            return documentInfo(cursor);
        }
    }

    private static DocumentInfo documentInfo(Cursor cursor) throws IOException {
        String id = cursor.getString(cursor.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_DOCUMENT_ID));
        String name = cursor.getString(cursor.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_DISPLAY_NAME));
        String mime = cursor.getString(cursor.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_MIME_TYPE));
        if (name == null || name.isEmpty()) {
            throw new IOException("The document provider returned an empty filename");
        }
        return new DocumentInfo(id, name, mime);
    }

    private String queryDisplayName(Uri uri) throws IOException {
        try (Cursor cursor = getContentResolver().query(uri, new String[]{OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (cursor == null || !cursor.moveToFirst()) {
                throw new IOException("The document provider returned no filename for " + uri);
            }
            String name = cursor.getString(cursor.getColumnIndexOrThrow(OpenableColumns.DISPLAY_NAME));
            if (name == null || name.isEmpty()) {
                throw new IOException("The document provider returned an empty filename");
            }
            return name;
        }
    }

    private static String[] mimeTypesFor(String[] extensions) {
        Set<String> mimeTypes = new LinkedHashSet<>();
        for (String extension : extensions) {
            switch (extension.toLowerCase(Locale.ROOT)) {
                case "epub":
                    mimeTypes.add("application/epub+zip");
                    break;
                case "pdf":
                    mimeTypes.add("application/pdf");
                    break;
                case "mobi":
                    mimeTypes.add("application/x-mobipocket-ebook");
                    break;
                case "azw":
                case "azw3":
                    mimeTypes.add("application/vnd.amazon.ebook");
                    mimeTypes.add("application/x-mobi8-ebook");
                    break;
                case "m4b":
                    mimeTypes.add("audio/mp4");
                    mimeTypes.add("audio/x-m4b");
                    break;
                default:
                    break;
            }
        }
        return mimeTypes.toArray(new String[0]);
    }

    private static Intent filePromptIntent(boolean multiple, String[] extensions) {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        String[] mimeTypes = mimeTypesFor(extensions);
        intent.setType(mimeTypes.length == 1 ? mimeTypes[0] : "*/*");
        if (mimeTypes.length > 1) {
            intent.putExtra(Intent.EXTRA_MIME_TYPES, mimeTypes);
        }
        intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, multiple);
        return intent;
    }
}
