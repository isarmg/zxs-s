package local.zxs.mobile;

import android.content.ContentResolver;
import android.content.Context;
import android.content.Intent;
import android.net.Uri;
import android.provider.OpenableColumns;
import android.webkit.MimeTypeMap;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.UUID;

record PickedFile(File file) implements AutoCloseable {
    static List<Uri> selection(Intent result) {
        List<Uri> selected = new ArrayList<>();
        var clip = result.getClipData();
        if (clip != null) {
            for (int index = 0; index < clip.getItemCount(); index++) {
                Uri uri = clip.getItemAt(index).getUri();
                if (uri != null) selected.add(uri);
            }
        } else if (result.getData() != null) selected.add(result.getData());
        return selected;
    }

    static String name(ContentResolver resolver, Uri uri, String prefix) {
        try (var cursor = resolver.query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            String name = cursor != null && cursor.moveToFirst() ? cursor.getString(0) : null;
            if (name != null && !name.isBlank()) return name;
            String mime = resolver.getType(uri);
            String extension = mime == null ? null : MimeTypeMap.getSingleton().getExtensionFromMimeType(mime);
            return generatedName(uri, prefix) + (extension == null ? "" : "." + extension);
        } catch (SecurityException unavailable) {
            return generatedName(uri, prefix);
        }
    }

    private static String generatedName(Uri uri, String prefix) {
        return prefix + UUID.nameUUIDFromBytes(uri.toString().getBytes(StandardCharsets.UTF_8));
    }

    static PickedFile stage(Context context, Uri uri) throws IOException {
        File temporary = File.createTempFile("zxs-import-", ".upload", context.getCacheDir());
        try (var input = context.getContentResolver().openInputStream(uri);
             var output = new FileOutputStream(temporary)) {
            if (input == null) throw new IOException("无法读取所选文件，请重新选择");
            input.transferTo(output);
        } catch (IOException | RuntimeException error) {
            temporary.delete();
            throw error;
        }
        return new PickedFile(temporary);
    }

    @Override public void close() { file.delete(); }
}
