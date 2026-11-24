package local.zxs.mobile;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;
import android.webkit.MimeTypeMap;
import java.io.File;
import java.io.FileNotFoundException;
import java.io.IOException;
import java.util.List;
import java.util.Locale;

public final class DocumentProvider extends ContentProvider {
    static final String AUTHORITY = "local.zxs.mobile.documents";
    static final String CACHE = "zxs-docs";

    @Override public boolean onCreate() { return true; }

    static Uri uri(File file) {
        return new Uri.Builder().scheme("content").authority(AUTHORITY)
            .appendPath(file.getParentFile().getName()).appendPath(file.getName()).build();
    }

    private File file(Uri uri) throws FileNotFoundException {
        List<String> path = uri.getPathSegments();
        if (path.size() != 2 || !path.get(0).matches("[0-9a-f]{32}")) throw new FileNotFoundException();
        File root = new File(getContext().getCacheDir(), CACHE);
        File dir = new File(root, path.get(0));
        try {
            File file = new File(dir, path.get(1)).getCanonicalFile();
            if (!file.getParentFile().equals(dir.getCanonicalFile()) || !file.isFile()) {
                throw new FileNotFoundException();
            }
            return file;
        } catch (IOException error) {
            throw new FileNotFoundException(error.getMessage());
        }
    }

    @Override public ParcelFileDescriptor openFile(Uri uri, String mode) throws FileNotFoundException {
        if (!mode.equals("r")) throw new FileNotFoundException("read-only");
        return ParcelFileDescriptor.open(file(uri), ParcelFileDescriptor.MODE_READ_ONLY);
    }

    @Override public String getType(Uri uri) {
        String extension = MimeTypeMap.getFileExtensionFromUrl(uri.toString()).toLowerCase(Locale.ROOT);
        String type = MimeTypeMap.getSingleton().getMimeTypeFromExtension(extension);
        return type == null ? "application/octet-stream" : type;
    }

    @Override public Cursor query(Uri uri, String[] projection, String selection, String[] args, String order) {
        String[] columns = projection == null ? new String[] { OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE } : projection;
        MatrixCursor result = new MatrixCursor(columns);
        try {
            File file = file(uri);
            Object[] values = new Object[columns.length];
            for (int i = 0; i < columns.length; i++) {
                if (columns[i].equals(OpenableColumns.DISPLAY_NAME)) values[i] = file.getName();
                else if (columns[i].equals(OpenableColumns.SIZE)) values[i] = file.length();
            }
            result.addRow(values);
        } catch (FileNotFoundException ignored) {}
        return result;
    }

    @Override public Uri insert(Uri uri, ContentValues data) { throw new UnsupportedOperationException(); }
    @Override public int delete(Uri uri, String filter, String[] args) { throw new UnsupportedOperationException(); }
    @Override public int update(Uri uri, ContentValues data, String filter, String[] args) { throw new UnsupportedOperationException(); }
}
