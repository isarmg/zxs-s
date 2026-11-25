package local.zxs.mobile;

import android.app.Activity;
import android.content.ClipData;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.provider.MediaStore;
import android.provider.OpenableColumns;
import android.text.InputType;
import android.view.ViewGroup;
import android.webkit.MimeTypeMap;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.MediaController;
import android.widget.VideoView;
import android.window.OnBackInvokedCallback;
import android.window.OnBackInvokedDispatcher;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.security.GeneralSecurityException;
import java.time.Instant;
import java.time.ZoneId;
import java.time.format.DateTimeFormatter;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.StringJoiner;
import java.util.UUID;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

public final class MainActivity extends Activity {
    private static final int PHOTO_PICKER = 1;
    private static final String ALBUM = "/相册";
    private static final DateTimeFormatter TIME = DateTimeFormatter.ofPattern("uuuu-MM-dd HH:mm", Locale.ROOT);
    private final ExecutorService worker = Executors.newSingleThreadExecutor();
    private SessionStore sessions;
    private String server;
    private String token;
    private String folder = "/";
    private Screen screen;
    private ArchiveList files;
    private VideoView video;
    private OnBackInvokedCallback systemBack;
    private boolean busy;

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        sessions = new SessionStore(this);
        server = sessions.server();
        token = "";
        try {
            token = sessions.load();
        } catch (GeneralSecurityException | IOException | IllegalArgumentException error) {
            sessions.clear();
            showLogin();
            screen.notice("登录凭据无法读取，请重新登录。", true);
            return;
        }
        if (token.isEmpty()) {
            showLogin();
        } else {
            if (state != null) folder = state.getString("folder", "/");
            showFolder();
            browse(folder, null, null);
        }
    }

    @Override protected void onSaveInstanceState(Bundle state) {
        super.onSaveInstanceState(state);
        state.putString("folder", folder);
    }

    @Override protected void onDestroy() {
        clearBackAction();
        if (video != null) video.stopPlayback();
        worker.shutdownNow();
        super.onDestroy();
    }

    private void reset(String title, String subtitle) {
        clearBackAction();
        if (video != null) {
            video.stopPlayback();
            video = null;
        }
        screen = new Screen(this, title, subtitle);
        setContentView(screen.root);
        files = null;
    }

    private void backAction(String title, Runnable action) {
        screen.back(title, action);
        systemBack = action::run;
        getOnBackInvokedDispatcher().registerOnBackInvokedCallback(
            OnBackInvokedDispatcher.PRIORITY_DEFAULT, systemBack);
    }

    private void clearBackAction() {
        if (systemBack == null) return;
        getOnBackInvokedDispatcher().unregisterOnBackInvokedCallback(systemBack);
        systemBack = null;
    }

    private void showLogin() {
        reset("ZXS", null);
        LinearLayout form = screen.scrollContent();
        int pad = screen.dp(16);
        form.setPadding(pad, pad, pad, pad);
        EditText endpoint = new EditText(this);
        endpoint.setHint("局域网服务器地址");
        endpoint.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI);
        endpoint.setText(server.isEmpty() ? "http://" : server);
        form.addView(endpoint);
        EditText password = new EditText(this);
        password.setHint("口令");
        password.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD);
        form.addView(password);
        screen.addPrimaryButton(form, "连接", () -> {
            String address = endpoint.getText().toString();
            String secret = password.getText().toString();
            background(() -> {
                JSONObject result = Bridge.call("login", new JSONObject().put("server", address).put("password", secret));
                sessions.save(result.getString("server"), result.getJSONObject("session").getString("token"));
                return result;
            }, result -> {
                server = result.getString("server");
                token = result.getJSONObject("session").getString("token");
                password.setText("");
                browse("/", null, null);
            });
        });
    }

    private void showFolder() {
        reset("ZXS", folder);
        files = screen.fileList();
        if (!folder.equals("/")) {
            int slash = folder.lastIndexOf('/');
            String parent = slash == 0 ? "/" : folder.substring(0, slash);
            backAction("上一级", () -> browse(parent, null, null));
        }
        screen.command(R.drawable.ic_refresh, "刷新", () -> browse(folder, null, null));
        screen.command(R.drawable.ic_exit, "退出", this::logout);
        screen.upload(this::pickPhotos);
    }

    private void browse(String requested, String next, String success) {
        background(() -> Bridge.call("browse", args().put("folder", requested).put("cursor", next)),
            page -> {
                if (next == null) {
                    folder = requested;
                    showFolder();
                }
                render(page, next == null);
                if (success != null) screen.notice(success, false);
            });
    }

    private void render(JSONObject page, boolean first) throws JSONException {
        JSONArray containers = page.getJSONArray("containers");
        JSONArray folders = page.getJSONArray("folders");
        List<ArchiveList.Entry> entries = new ArrayList<>();
        if (first) {
            for (int i = 0; i < folders.length(); i++) {
                JSONObject entry = folders.getJSONObject(i);
                String child = entry.getString("folder");
                String name = child.substring(child.lastIndexOf('/') + 1);
                entries.add(new ArchiveList.Entry(name, entry.getLong("count") + " 项", Screen.FileKind.FOLDER,
                    () -> browse(child, null, null)));
            }
        }
        for (int i = 0; i < containers.length(); i++) {
            entries.add(fileEntry(containers.getJSONObject(i)));
        }
        String cursor = page.isNull("next_cursor") ? null : page.getString("next_cursor");
        files.append(entries, cursor == null ? null : () -> browse(folder, cursor, null));
        if (first && entries.isEmpty()) screen.showEmptyList(files);
    }

    private ArchiveList.Entry fileEntry(JSONObject entry) throws JSONException {
        String id = entry.getString("container_id");
        String name = entry.getString("name");
        var updated = Instant.ofEpochSecond(entry.getLong("updated_at")).atZone(ZoneId.systemDefault());
        var subtitle = new StringJoiner(" · ").add("v" + entry.getLong("latest_version")).add(TIME.format(updated));
        JSONArray tags = entry.getJSONArray("tags");
        for (int i = 0; i < tags.length(); i++) subtitle.add(tags.getString(i));
        return new ArchiveList.Entry(name, subtitle.toString(), Screen.FileKind.of(name), () -> open(id));
    }

    private void open(String id) {
        File cache = new File(getCacheDir(), DocumentProvider.CACHE);
        background(() -> Bridge.call("open", args().put("id", id).put("cache", cache.getAbsolutePath())), data -> {
            if (data.has("url")) play(data.getString("url"));
            else viewDocument(new File(data.getString("path")), data.getString("type"));
        });
    }

    private void play(String url) {
        reset("原件播放", null);
        backAction("返回文件列表", () -> {
            showFolder();
            browse(folder, null, null);
        });
        video = new VideoView(this);
        video.setLayoutParams(new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, screen.dp(360)));
        screen.scrollContent().addView(video);
        MediaController controls = new MediaController(this);
        controls.setAnchorView(video);
        video.setMediaController(controls);
        video.setOnErrorListener((player, error, extra) -> {
            screen.notice("系统播放器无法播放此文件。", true);
            return true;
        });
        video.setVideoURI(Uri.parse(url));
        video.start();
    }

    private void viewDocument(File file, String type) {
        Uri uri = DocumentProvider.uri(file);
        Intent intent = new Intent(Intent.ACTION_VIEW);
        String mime = type.equals("application/octet-stream") ? getContentResolver().getType(uri) : type;
        intent.setDataAndType(uri, mime);
        intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        startActivity(Intent.createChooser(intent, "查看文件"));
    }

    private void logout() {
        background(() -> {
            try { return Bridge.call("logout", args()); }
            catch (Bridge.Failure failure) {
                if (failure.status != 401) throw failure;
                return new JSONObject();
            }
        }, ignored -> {
            token = "";
            sessions.clear();
            showLogin();
        });
    }

    private void pickPhotos() {
        if (busy) return;
        Intent intent = new Intent(MediaStore.ACTION_PICK_IMAGES);
        intent.putExtra(MediaStore.EXTRA_PICK_IMAGES_MAX, Math.min(64, MediaStore.getPickImagesMaxLimit()));
        startActivityForResult(intent, PHOTO_PICKER);
    }

    @Override protected void onActivityResult(int request, int result, Intent intent) {
        super.onActivityResult(request, result, intent);
        if (request != PHOTO_PICKER || result != RESULT_OK || intent == null) return;
        List<Uri> selected = new ArrayList<>();
        ClipData clip = intent.getClipData();
        if (clip != null) {
            for (int i = 0; i < clip.getItemCount(); i++) selected.add(clip.getItemAt(i).getUri());
        } else if (intent.getData() != null) selected.add(intent.getData());
        background(() -> {
            for (Uri uri : selected) backup(uri);
            return selected.size();
        }, count -> browse(ALBUM, null, "已检查并备份 " + count + " 个项目"));
    }

    private void backup(Uri uri) throws Exception {
        String name = displayName(uri);
        File temporary = File.createTempFile("zxs-import-", ".upload", getCacheDir());
        try {
            try (InputStream input = getContentResolver().openInputStream(uri);
                 FileOutputStream output = new FileOutputStream(temporary)) {
                if (input == null) throw new IOException("无法打开媒体文件");
                input.transferTo(output);
            }
            Bridge.call("backup", args().put("file", temporary.getAbsolutePath()).put("name", name).put("folder", ALBUM));
        } finally {
            temporary.delete();
        }
    }

    private String displayName(Uri uri) {
        try (Cursor cursor = getContentResolver().query(uri, new String[] { OpenableColumns.DISPLAY_NAME }, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) {
                String name = cursor.getString(0);
                if (name != null && !name.isBlank()) return name;
            }
        }
        UUID id = UUID.nameUUIDFromBytes(uri.toString().getBytes(StandardCharsets.UTF_8));
        String mime = getContentResolver().getType(uri);
        String extension = mime == null ? null : MimeTypeMap.getSingleton().getExtensionFromMimeType(mime);
        return "media-" + id + (extension == null ? "" : "." + extension);
    }

    private JSONObject args() throws Exception {
        return new JSONObject().put("connection", new JSONObject().put("server", server).put("token", token));
    }

    private interface Work<T> { T run() throws Exception; }
    private interface Done<T> { void run(T value) throws Exception; }

    private <T> void background(Work<T> work, Done<T> done) {
        if (busy) return;
        busy = true;
        screen.busy(true);
        worker.submit(() -> {
            try {
                T value = work.run();
                runOnUiThread(() -> finish(() -> done.run(value)));
            } catch (Exception error) {
                runOnUiThread(() -> finish(() -> { throw error; }));
            }
        });
    }

    private void finish(WorkDone done) {
        if (isDestroyed()) return;
        busy = false;
        screen.busy(false);
        try { done.run(); } catch (Exception error) {
            String message = error.getMessage();
            screen.notice(message == null ? "操作失败" : message, true);
        }
    }

    private interface WorkDone { void run() throws Exception; }
}
