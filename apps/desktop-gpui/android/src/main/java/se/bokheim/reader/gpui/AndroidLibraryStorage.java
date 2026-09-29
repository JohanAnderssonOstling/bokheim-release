package se.bokheim.reader.gpui;

import android.content.Context;
import android.net.Uri;
import android.os.Environment;
import android.os.Build;
import android.os.storage.StorageManager;
import android.os.storage.StorageVolume;
import android.provider.DocumentsContract;
import java.io.File;
import java.io.IOException;

final class AndroidLibraryStorage {
    private AndroidLibraryStorage() {}

    static File localDirectory(Context context, Uri tree) throws IOException {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R || !AllFilesAccess.isGranted() || !"com.android.externalstorage.documents".equals(tree.getAuthority())) {
            return null;
        }
        String[] document = DocumentsContract.getTreeDocumentId(tree).split(":", 2);
        File volumeRoot = null;
        if ("primary".equals(document[0])) {
            volumeRoot = Environment.getExternalStorageDirectory();
        } else {
            StorageManager manager = context.getSystemService(StorageManager.class);
            for (StorageVolume volume : manager.getStorageVolumes()) {
                if (document[0].equalsIgnoreCase(volume.getUuid())) {
                    volumeRoot = volume.getDirectory();
                    break;
                }
            }
        }
        return volumeRoot == null ? null : accessibleChild(volumeRoot, document.length > 1 ? document[1] : "");
    }

    static File accessibleChild(File volumeRoot, String relative) throws IOException {
        File root = volumeRoot.getCanonicalFile();
        File folder = new File(root, relative).getCanonicalFile();
        if (!folder.equals(root) && !folder.getPath().startsWith(root.getPath() + File.separator)) {
            throw new IOException("Selected folder is outside the storage volume");
        }
        return folder.isDirectory() && folder.canRead() && folder.canWrite() ? folder : null;
    }
}
