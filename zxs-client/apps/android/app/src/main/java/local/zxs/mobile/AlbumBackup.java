package local.zxs.mobile;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.Intent;
import android.net.Uri;
import android.provider.MediaStore;
import java.util.ArrayList;
import java.util.List;
import java.util.UUID;
import java.util.function.Consumer;
import java.util.function.IntConsumer;
import java.util.stream.Collectors;
import org.json.JSONObject;

final class AlbumBackup {
    static final int PHOTO_PICKER = 1;

    private AlbumBackup() {}

    private enum Status { Saved, Present, Failed }

    record Destination(String folder, JSONObject request) {}

    record Outcome(Uri source, String name, Status status, String detail) {
        String label() {
            String result = switch (status) {
                case Saved -> "已上传";
                case Present -> "已存在，跳过";
                case Failed -> "失败";
            };
            return name + "\n" + result + (detail.isEmpty() ? "" : " · " + detail);
        }
    }

    static void openPicker(Activity activity) {
        Intent intent = new Intent(MediaStore.ACTION_PICK_IMAGES);
        intent.putExtra(MediaStore.EXTRA_PICK_IMAGES_MAX, MediaStore.getPickImagesMaxLimit());
        activity.startActivityForResult(intent, PHOTO_PICKER);
    }

    static List<Outcome> upload(Activity activity, List<Uri> selected, Destination target, IntConsumer progress) {
        List<Outcome> outcomes = new ArrayList<>(selected.size());
        String batch = selected.size() > 1 ? UUID.randomUUID().toString() : null;
        for (Uri uri : selected) {
            String name = "第 " + (outcomes.size() + 1) + " 个项目";
            try {
                name = PickedFile.name(activity.getContentResolver(), uri, "media-");
                try (var staged = PickedFile.stage(activity, uri)) {
                    var request = target.request().put("folder", target.folder()).put("batch", batch);
                    request.put("file", staged.file().getAbsolutePath()).put("name", name);
                    var data = Bridge.call("backup", request);
                    String stored = data.getJSONObject("container").getString("name");
                    Status status = data.optBoolean("already_present") ? Status.Present : Status.Saved;
                    outcomes.add(new Outcome(uri, name, status, name.equals(stored) ? "" : stored));
                }
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
            .setTitle("相册上传结果\n已上传 " + saved + " · 跳过 " + skipped + " · 失败 " + failed.size())
            .setItems(outcomes.stream().map(Outcome::label).toArray(String[]::new), null)
            .setPositiveButton("完成", (window, button) -> {
                if (saved + skipped > 0) done.run();
            });
        if (!failed.isEmpty()) dialog.setNegativeButton("重试失败项", (window, button) -> retry.accept(failed));
        dialog.show();
    }
}
