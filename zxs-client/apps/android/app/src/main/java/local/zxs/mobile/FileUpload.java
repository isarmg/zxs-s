package local.zxs.mobile;

import android.app.AlertDialog;
import android.content.Intent;
import android.net.Uri;
import android.os.Bundle;
import android.provider.MediaStore;
import android.widget.EditText;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.stream.Collectors;
import org.json.JSONException;
import org.json.JSONObject;

final class FileUpload {
    private static final int PHOTO_PICKER = 1;
    private static final int DOCUMENT_PICKER = 2;
    private final MainActivity activity;
    private Target picking;

    FileUpload(MainActivity activity) { this.activity = activity; }

    private record Target(String folder, String name, String id, long version) {
        boolean existing() { return id != null; }

        JSONObject fields(boolean photo) throws JSONException {
            var target = new JSONObject();
            if (existing()) {
                return target.put("kind", "version").put("container_id", id).put("expected_version", version);
            }
            return target.put("kind", photo ? "photo" : "new").put("folder", folder)
                .put("name", name);
        }

        static Target current(JSONObject data) throws JSONException {
            return new Target(data.getString("folder"), data.getString("name"),
                data.getString("container_id"), data.getLong("latest_version"));
        }

        String location() { return folder + "/" + name; }
    }

    private record Item(Uri uri, String name, Target target) {
        Item at(Target destination) { return new Item(uri, name, destination); }
    }

    private enum Status {
        SAVED("已提交"), PRESENT("已存在"), CONFLICT("冲突，点按处理"), FAILED("失败"), SKIPPED("已跳过");

        private final String title;
        Status(String title) { this.title = title; }
    }

    private record Outcome(Item item, Status status, String detail, Target current) {
        String label() {
            return item.name() + "\n" + status.title + (detail.isEmpty() ? "" : " · " + detail);
        }
    }

    void save(Bundle state) {
        if (picking == null) return;
        state.putString("uploadFolder", picking.folder());
        state.putString("uploadName", picking.name());
        state.putString("uploadId", picking.id());
        state.putLong("uploadVersion", picking.version());
    }

    void restore(Bundle state) {
        if (state.containsKey("uploadFolder")) {
            picking = new Target(state.getString("uploadFolder"), state.getString("uploadName"),
                state.getString("uploadId"), state.getLong("uploadVersion"));
        }
    }

    void menu(String folder) {
        new AlertDialog.Builder(activity).setTitle("上传")
            .setItems(new String[] {"上传文件", "上传照片和视频"}, (dialog, index) -> {
                pick(new Target(folder, null, null, 0), index == 1);
            }).show();
    }

    void fileMenu(String folder, ArchiveList.Container file) {
        new AlertDialog.Builder(activity).setTitle(file.name())
            .setItems(new String[] {"上传新版本"}, (dialog, index) ->
                pick(new Target(folder, file.name(), file.id(), file.version()), false))
            .show();
    }

    private void pick(Target target, boolean photo) {
        picking = target;
        if (photo) {
            Intent intent = new Intent(MediaStore.ACTION_PICK_IMAGES);
            intent.putExtra(MediaStore.EXTRA_PICK_IMAGES_MAX, MediaStore.getPickImagesMaxLimit());
            activity.startActivityForResult(intent, PHOTO_PICKER);
            return;
        }
        var intent = new Intent(Intent.ACTION_OPEN_DOCUMENT).setType("*/*");
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, !target.existing());
        activity.startActivityForResult(intent, DOCUMENT_PICKER);
    }

    void receive(int request, Intent intent) {
        if (request != PHOTO_PICKER && request != DOCUMENT_PICKER) return;
        Target target = picking;
        picking = null;
        if (target == null || intent == null) return;
        List<Uri> selected = PickedFile.selection(intent);
        if (selected.isEmpty() || (target.existing() && selected.size() != 1)) return;
        boolean photos = request == PHOTO_PICKER;
        activity.afterWork(() -> activity.background(() -> describe(selected, target, photos), items -> {
            if (photos) new Batch(true).submit(items);
            else confirm(items, target);
        }));
    }

    private List<Item> describe(List<Uri> selected, Target target, boolean photos) {
        List<Item> items = new ArrayList<>(selected.size());
        for (Uri uri : selected) {
            String name = PickedFile.name(activity.getContentResolver(), uri, photos ? "media-" : "file-");
            var destination = target.existing() ? target : new Target(target.folder(), name, null, 0);
            items.add(new Item(uri, name, destination));
        }
        return items;
    }

    private void confirm(List<Item> items, Target target) {
        String destination = target.existing() ? target.location() + " · 当前 v" + target.version() : target.folder();
        new AlertDialog.Builder(activity).setTitle("上传到 " + destination)
            .setItems(items.stream().map(Item::name).toArray(String[]::new), null)
            .setNegativeButton("取消", null)
            .setPositiveButton("上传", (dialog, button) -> new Batch(false).submit(items))
            .show();
    }

    private final class Batch {
        private final boolean photos;
        private final LinkedHashMap<Uri, Outcome> results = new LinkedHashMap<>();

        Batch(boolean photos) { this.photos = photos; }

        void submit(List<Item> items) {
            activity.background(() -> transfer(items, photos), outcomes -> {
                outcomes.forEach(result -> results.put(result.item().uri(), result));
                show();
            });
        }

        private void show() {
            var outcomes = List.copyOf(results.values());
            var failed = outcomes.stream().filter(result -> result.status() == Status.FAILED)
                .map(Outcome::item).collect(Collectors.toList());
            String title = "上传结果";
            if (photos) {
                long saved = outcomes.stream().filter(result -> result.status() == Status.SAVED).count();
                long skipped = outcomes.stream().filter(result -> result.status() == Status.PRESENT).count();
                title += "\n已上传 " + saved + " · 跳过 " + skipped + " · 失败 " + failed.size();
            }
            var dialog = new AlertDialog.Builder(activity).setTitle(title)
                .setItems(outcomes.stream().map(Outcome::label).toArray(String[]::new), null)
                .setOnCancelListener(ignored -> refresh())
                .setPositiveButton("完成", (window, button) -> refresh());
            if (!failed.isEmpty()) dialog.setNegativeButton("重试失败项", (window, button) -> submit(failed));
            var opened = dialog.show();
            opened.getListView().setOnItemClickListener((list, view, index, id) -> {
                if (outcomes.get(index).status() != Status.CONFLICT) return;
                opened.dismiss();
                conflict(outcomes.get(index));
            });
        }

        private void refresh() {
            if (results.values().stream().anyMatch(result -> result.status() == Status.SAVED
                    || result.status() == Status.PRESENT)) activity.reloadList();
        }

        private void conflict(Outcome result) {
            Item item = result.item();
            Target current = result.current();
            var dialog = new AlertDialog.Builder(activity).setTitle("处理冲突 · " + item.name())
                .setMessage(result.detail()).setOnCancelListener(ignored -> show())
                .setNegativeButton("跳过", (window, button) -> {
                    results.put(item.uri(), new Outcome(item, Status.SKIPPED, "", null));
                    show();
                }).setNeutralButton("改名另存", (window, button) -> rename(item));
            if (current != null) dialog.setPositiveButton("追加到最新版", (window, button) ->
                submit(List.of(item.at(current))));
            dialog.show();
        }

        private void rename(Item item) {
            var name = new EditText(activity);
            name.setText(item.target().name());
            name.setSelection(name.length());
            new AlertDialog.Builder(activity).setTitle("另存到 " + item.target().folder())
                .setView(name).setOnCancelListener(ignored -> show())
                .setNegativeButton("返回结果", (dialog, button) -> show())
                .setPositiveButton("另存", (dialog, button) -> {
                    var target = new Target(item.target().folder(), name.getText().toString(), null, 0);
                    submit(List.of(item.at(target)));
                }).show();
        }
    }

    private List<Outcome> transfer(List<Item> items, boolean photos) throws Exception {
        List<Outcome> outcomes = new ArrayList<>(items.size());
        for (Item item : items) {
            if (Thread.currentThread().isInterrupted()) break;
            String label = (outcomes.size() + 1) + " / " + items.size() + " · " + item.name();
            outcomes.add(transferOne(item, label, photos));
        }
        return outcomes;
    }

    private Outcome transferOne(Item item, String label, boolean photos) throws Bridge.Failure {
        activity.uploadProgress("读取文件 " + label);
        try (var staged = PickedFile.stage(activity, item.uri())) {
            activity.uploadProgress("上传文件 " + label);
            var request = activity.args().put("target", item.target().fields(photos))
                .put("file", staged.file().getAbsolutePath());
            return outcome(item, Bridge.call("commit_file", request));
        } catch (Exception error) {
            if (error instanceof Bridge.Failure failure && failure.passwordRejected) throw failure;
            String reason = error instanceof SecurityException ? "无法读取所选文件，请重新选择" : error.getMessage();
            return new Outcome(item, Status.FAILED, reason == null ? "操作失败" : reason, null);
        }
    }

    private static Outcome outcome(Item item, JSONObject reply) throws JSONException {
        Status status = switch (reply.getString("outcome")) {
            case "committed" -> Status.SAVED;
            case "already_present" -> Status.PRESENT;
            case "conflict" -> Status.CONFLICT;
            default -> throw new JSONException("服务器返回了未知的提交结果");
        };
        JSONObject container = status == Status.SAVED
            ? reply.getJSONObject("commit").getJSONObject("container") : reply.optJSONObject("current");
        if (container == null) return new Outcome(item, status, reply.getString("message"), null);
        Target current = Target.current(container);
        return new Outcome(item, status, current.location() + " · v" + current.version(), current);
    }
}
