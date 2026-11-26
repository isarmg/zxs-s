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
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.UUID;
import java.util.function.Consumer;
import java.util.function.IntConsumer;
import java.util.stream.Collectors;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

final class AlbumBackup {
    private AlbumBackup() {}

    private enum Status { Saved, Present, Failed }

    record Destination(String folder, JSONObject request) {}

    record Outcome(Uri source, String name, Status status, String detail) {
        String label() {
            String result = switch (status) {
                case Saved -> "已备份";
                case Present -> "已存在，跳过";
                case Failed -> "失败";
            };
            return name + "\n" + result + (detail.isEmpty() ? "" : " · " + detail);
        }
    }

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

    static List<Outcome> upload(Activity activity, List<Uri> selected, Destination target, IntConsumer progress) {
        List<Outcome> outcomes = new ArrayList<>(selected.size());
        String batch = selected.size() > 1 ? UUID.randomUUID().toString() : null;
        for (Uri uri : selected) {
            String name = "第 " + (outcomes.size() + 1) + " 个项目";
            try {
                name = displayName(activity, uri);
                var request = target.request().put("folder", target.folder()).put("batch", batch);
                var data = uploadFile(activity, uri, request, name);
                String stored = data.getJSONObject("container").getString("name");
                Status status = data.optBoolean("already_present") ? Status.Present : Status.Saved;
                outcomes.add(new Outcome(uri, name, status, name.equals(stored) ? "" : stored));
            } catch (Exception error) {
                String reason = error.getMessage();
                outcomes.add(new Outcome(uri, name, Status.Failed, reason == null ? "操作失败" : reason));
            }
            progress.accept(outcomes.size());
        }
        return outcomes;
    }

    static void show(Activity activity, List<Outcome> outcomes, Runnable done, Consumer<List<Uri>> retry) {
        List<Uri> failed = outcomes.stream().filter(item -> item.status == Status.Failed)
            .map(Outcome::source).collect(Collectors.toList());
        long saved = outcomes.stream().filter(item -> item.status == Status.Saved).count();
        long skipped = outcomes.size() - saved - failed.size();
        var dialog = new AlertDialog.Builder(activity)
            .setTitle("相册备份结果\n已备份 " + saved + " · 跳过 " + skipped + " · 失败 " + failed.size())
            .setItems(outcomes.stream().map(Outcome::label).toArray(String[]::new), null)
            .setPositiveButton("完成", (window, button) -> {
                if (saved + skipped > 0) done.run();
            });
        if (!failed.isEmpty()) dialog.setNegativeButton("重试失败项", (window, button) -> retry.accept(failed));
        dialog.show();
    }

    private static JSONObject uploadFile(Activity activity, Uri uri, JSONObject request, String name)
            throws Exception {
        File temporary = File.createTempFile("zxs-import-", ".upload", activity.getCacheDir());
        try {
            try (InputStream input = activity.getContentResolver().openInputStream(uri);
                 FileOutputStream output = new FileOutputStream(temporary)) {
                if (input == null) throw new IOException("无法打开媒体文件");
                input.transferTo(output);
            }
            return Bridge.call("backup", request.put("file", temporary.getAbsolutePath()).put("name", name));
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
