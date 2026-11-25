package local.zxs.mobile;

import android.app.Activity;
import android.content.ClipData;
import android.content.Intent;
import android.net.Uri;
import android.os.Bundle;
import android.provider.MediaStore;
import android.text.InputType;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.VideoView;
import android.window.OnBackInvokedCallback;
import android.window.OnBackInvokedDispatcher;
import java.io.File;
import java.io.IOException;
import java.security.GeneralSecurityException;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

public final class MainActivity extends Activity {
    private static final int PHOTO_PICKER = 1;
    private final ExecutorService worker = Executors.newSingleThreadExecutor();
    private SessionStore sessions;
    private String server;
    private String savedPassword;
    private String folder = "/";
    private BundleDirectory bundle;
    private String album;
    private Screen screen;
    private ArchiveList files;
    private VideoView video;
    private OnBackInvokedCallback systemBack;
    private boolean busy;

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        sessions = new SessionStore(this);
        server = "";
        try {
            SessionStore.Account account = sessions.load();
            server = account.server();
            savedPassword = server.isEmpty() ? null : account.password();
        } catch (GeneralSecurityException | IOException | IllegalArgumentException error) {
            sessions.clear();
            showLogin();
            screen.notice("登录凭据无法读取，请重新登录。", true);
            return;
        }
        if (state != null) album = state.getString("album");
        if (savedPassword == null) {
            showLogin();
        } else {
            if (state != null) {
                folder = state.getString("folder", "/");
                bundle = BundleDirectory.restore(state);
            }
            reloadList();
        }
    }

    @Override protected void onSaveInstanceState(Bundle state) {
        super.onSaveInstanceState(state);
        state.putString("folder", folder);
        state.putString("album", album);
        if (bundle != null) bundle.save(state);
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
                sessions.save(result.getString("server"), secret);
                return result;
            }, result -> {
                server = result.getString("server");
                savedPassword = secret;
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
                    bundle = null;
                    showFolder();
                }
                render(page, next == null);
                if (success != null) screen.notice(success, false);
            });
    }

    private void render(JSONObject page, boolean first) throws JSONException {
        JSONArray containers = page.getJSONArray("containers");
        JSONArray folders = page.getJSONArray("folders");
        Set<String> bundles = ids(page.getJSONArray("bundles"));
        Set<String> conflicts = ids(page.getJSONArray("conflicts"));
        List<ArchiveList.Entry> entries = new ArrayList<>();
        if (first) {
            for (int i = 0; i < folders.length(); i++) {
                JSONObject entry = folders.getJSONObject(i);
                String child = entry.getString("folder");
                String name = child.substring(child.lastIndexOf('/') + 1);
                Screen.FileKind kind = folder.equals("/") ? Screen.FileKind.DISK : Screen.FileKind.FOLDER;
                entries.add(new ArchiveList.Entry(name, "", kind,
                    () -> browse(child, null, null)));
            }
        }
        for (int i = 0; i < containers.length(); i++) {
            entries.add(ArchiveList.file(containers.getJSONObject(i), bundles, conflicts, this::selectFile));
        }
        String cursor = page.isNull("next_cursor") ? null : page.getString("next_cursor");
        files.append(entries, cursor == null ? null : () -> browse(folder, cursor, null));
        if (first && entries.isEmpty()) screen.showEmptyList(files);
    }

    private Set<String> ids(JSONArray input) throws JSONException {
        Set<String> result = new HashSet<>();
        for (int i = 0; i < input.length(); i++) result.add(input.getString(i));
        return result;
    }

    private void selectFile(ArchiveList.Container entry) {
        if (entry.bundle()) browseBundle(BundleDirectory.root(entry), null);
        else open(entry);
    }

    private void showBundle() {
        BundleDirectory location = bundle;
        reset("ZXS", location.displayPath(folder));
        files = screen.fileList();
        backAction("上一级", () -> {
            if (location.isRoot()) browse(folder, null, null);
            else browseBundle(location.parent(), null);
        });
        screen.command(R.drawable.ic_refresh, "刷新", () -> browseBundle(location, null));
        screen.command(R.drawable.ic_exit, "退出", this::logout);
    }

    private void browseBundle(BundleDirectory location, String next) {
        background(() -> location.browse(args(), next), page -> {
            if (next == null) {
                bundle = location;
                showBundle();
            }
            var rows = page.rows(path -> browseBundle(location.at(path), null), path -> openBundleMember(location, path));
            String cursor = page.cursor();
            files.append(rows, cursor == null ? null : () -> browseBundle(location, cursor));
            if (next == null && rows.isEmpty()) screen.showEmptyList(files);
        });
    }

    private void reloadList() {
        if (bundle == null) {
            showFolder();
            browse(folder, null, null);
        } else {
            showBundle();
            browseBundle(bundle, null);
        }
    }

    private void open(ArchiveList.Container entry) {
        File cache = new File(getCacheDir(), DocumentProvider.CACHE);
        background(() -> Bridge.call("open", args().put("id", entry.id()).put("version", entry.version())
                .put("cache", cache.getAbsolutePath())), data -> {
            if (data.optBoolean("bundle")) browseBundle(BundleDirectory.root(entry), null);
            else if (data.has("url")) play(Uri.parse(data.getString("url")),
                Map.of("Authorization", data.getJSONObject("headers").getString("Authorization")));
            else FilePreview.document(this, new File(data.getString("path")), data.getString("type"));
        });
    }

    private void openBundleMember(BundleDirectory location, String path) {
        File cache = new File(getCacheDir(), DocumentProvider.CACHE);
        background(() -> location.open(args(), path, cache), data -> {
            File file = new File(data.getString("path"));
            String type = data.getString("type");
            if (type.startsWith("video/") || type.startsWith("audio/")) play(DocumentProvider.uri(file), Map.of());
            else FilePreview.document(this, file, type);
        });
    }

    private void play(Uri url, Map<String, String> headers) {
        reset("原件播放", null);
        backAction("返回文件列表", this::reloadList);
        video = FilePreview.play(this, screen, url, headers);
    }

    private void logout() {
        background(() -> {
            try { return Bridge.call("logout", args()); }
            catch (Bridge.Failure failure) {
                if (failure.status != 401) throw failure;
                return new JSONObject();
            }
        }, ignored -> {
            savedPassword = null;
            bundle = null;
            sessions.clear();
            showLogin();
        });
    }

    private void pickPhotos() {
        background(() -> Bridge.call("browse", args().put("folder", "/")),
            page -> AlbumBackup.choose(this, page.getJSONArray("folders"), server, this::openPhotoPicker));
    }

    private void openPhotoPicker(String target) {
        album = target;
        Intent intent = new Intent(MediaStore.ACTION_PICK_IMAGES);
        intent.putExtra(MediaStore.EXTRA_PICK_IMAGES_MAX, MediaStore.getPickImagesMaxLimit());
        startActivityForResult(intent, PHOTO_PICKER);
    }

    @Override protected void onActivityResult(int request, int result, Intent intent) {
        super.onActivityResult(request, result, intent);
        if (request != PHOTO_PICKER || result != RESULT_OK || intent == null || album == null) return;
        List<Uri> selected = new ArrayList<>();
        ClipData clip = intent.getClipData();
        if (clip != null) {
            for (int i = 0; i < clip.getItemCount(); i++) selected.add(clip.getItemAt(i).getUri());
        } else if (intent.getData() != null) selected.add(intent.getData());
        String target = album;
        background(() -> {
            for (Uri uri : selected) AlbumBackup.upload(this, uri, target, args());
            return selected.size();
        }, count -> browse(target, null, "已检查并备份 " + count + " 个项目"));
    }

    private JSONObject args() throws Exception {
        return new JSONObject().put("connection", new JSONObject().put("server", server).put("password", savedPassword));
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
            if (error instanceof Bridge.Failure failure && failure.status == 401 && savedPassword != null) {
                savedPassword = null;
                bundle = null;
                sessions.clear();
                showLogin();
            }
            String message = error.getMessage();
            screen.notice(message == null ? "操作失败" : message, true);
        }
    }

    private interface WorkDone { void run() throws Exception; }
}
