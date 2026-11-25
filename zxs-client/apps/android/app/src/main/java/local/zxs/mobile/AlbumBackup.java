package local.zxs.mobile;

import android.app.Activity;
import android.app.AlertDialog;
import android.database.Cursor;
import android.net.Uri;
import android.provider.OpenableColumns;
import android.webkit.MimeTypeMap;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.UUID;
import java.util.function.Consumer;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

final class AlbumBackup {
    private AlbumBackup() {}

    static void choose(Activity activity, JSONArray entries, String server, Consumer<String> start)
            throws JSONException, IOException {
        if (entries.length() == 0) throw new IOException("服务器没有可用于备份的数据盘");
        String[] disks = new String[entries.length()];
        for (int i = 0; i < disks.length; i++) disks[i] = entries.getJSONObject(i).getString("folder");
        if (disks.length == 1) {
            start.accept(disks[0] + "/相册");
            return;
        }
        var preferences = activity.getSharedPreferences("album-disks", Activity.MODE_PRIVATE);
        String saved = preferences.getString(server, "");
        String[] labels = Arrays.stream(disks).map(disk -> disk.substring(1)).toArray(String[]::new);
        int selected = Math.max(0, Arrays.asList(disks).indexOf(saved));
        new AlertDialog.Builder(activity)
            .setTitle("相册备份数据盘")
            .setSingleChoiceItems(labels, selected, null)
            .setNegativeButton("取消", null)
            .setPositiveButton("选择照片", (dialog, button) -> {
                int position = ((AlertDialog) dialog).getListView().getCheckedItemPosition();
                String disk = disks[Math.max(0, position)];
                preferences.edit().putString(server, disk).apply();
                start.accept(disk + "/相册");
            })
            .show();
    }

    static void upload(Activity activity, Uri uri, String target, JSONObject args) throws Exception {
        String name = displayName(activity, uri);
        File temporary = File.createTempFile("zxs-import-", ".upload", activity.getCacheDir());
        try {
            try (InputStream input = activity.getContentResolver().openInputStream(uri);
                 FileOutputStream output = new FileOutputStream(temporary)) {
                if (input == null) throw new IOException("无法打开媒体文件");
                input.transferTo(output);
            }
            Bridge.call("backup", args.put("file", temporary.getAbsolutePath()).put("name", name).put("folder", target));
        } finally {
            temporary.delete();
        }
    }

    private static String displayName(Activity activity, Uri uri) {
        var resolver = activity.getContentResolver();
        try (Cursor cursor = resolver.query(uri, new String[] { OpenableColumns.DISPLAY_NAME }, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) {
                String name = cursor.getString(0);
                if (name != null && !name.isBlank()) return name;
            }
        }
        UUID id = UUID.nameUUIDFromBytes(uri.toString().getBytes(StandardCharsets.UTF_8));
        String mime = resolver.getType(uri);
        String extension = mime == null ? null : MimeTypeMap.getSingleton().getExtensionFromMimeType(mime);
        return "media-" + id + (extension == null ? "" : "." + extension);
    }
}
