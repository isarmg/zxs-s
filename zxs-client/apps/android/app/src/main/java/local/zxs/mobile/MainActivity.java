package local.zxs.mobile;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.text.InputType;
import android.widget.LinearLayout;
import android.widget.VideoView;
import android.window.OnBackInvokedCallback;
import android.window.OnBackInvokedDispatcher;
import java.io.File;
import java.io.IOException;
import java.security.GeneralSecurityException;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.concurrent.Callable;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

public final class MainActivity extends Activity {
    private final ExecutorService worker = Executors.newSingleThreadExecutor();
    private final FileUpload uploads = new FileUpload(this);
    private SessionStore sessions;
    private String server;
    private String savedPassword;
    private String folder = "/";
    private BundleDirectory bundle;
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
            screen.notice("登录凭据无法读取，请重新登录。");
            return;
        }
        if (savedPassword == null) {
            showLogin();
        } else {
            if (state != null) {
                folder = state.getString("folder", "/");
                bundle = BundleDirectory.restore(state);
                uploads.restore(state);
            }
            reloadList();
        }
    }

    @Override protected void onSaveInstanceState(Bundle state) {
        super.onSaveInstanceState(state);
        state.putString("folder", folder);
        if (bundle != null) bundle.save(state);
        uploads.save(state);
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
        reset(getString(R.string.app_name), null);
        LinearLayout form = screen.scrollContent();
        int pad = screen.dp(16);
        form.setPadding(pad, pad, pad, pad);
        var endpoint = screen.input(form, "局域网服务器地址");
        endpoint.setSingleLine(true);
        endpoint.setText(server.isEmpty() ? "http://" : server);
        var password = screen.input(form, "口令");
        password.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD);
        form.addView(screen.button("连接", true, () -> {
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
                folder = "/";
                reloadList();
            });
        }));
    }

    private void showList() {
        BundleDirectory location = bundle;
        reset(getString(R.string.app_name), location == null ? folder : location.displayPath(folder));
        files = screen.fileList();
        if (location == null) {
            if (!folder.equals("/")) {
                int slash = folder.lastIndexOf('/');
                String parent = slash == 0 ? "/" : folder.substring(0, slash);
                backAction("上一级", () -> browse(parent, null));
                screen.upload("上传", () -> { if (!busy) uploads.menu(folder); });
            }
            screen.command(R.drawable.ic_refresh, "刷新", () -> browse(folder, null));
        } else {
            backAction("上一级", () -> {
                if (location.isRoot()) browse(folder, null);
                else browseBundle(location.parent(), null);
            });
            screen.command(R.drawable.ic_refresh, "刷新", () -> browseBundle(location.atVersion(0), null));
        }
        screen.command(R.drawable.ic_exit, "退出", this::logout);
    }

    private void browse(String requested, String next) {
        background(() -> Bridge.call("browse", args().put("folder", requested).put("cursor", next)),
            page -> {
                if (next == null) {
                    folder = requested;
                    bundle = null;
                    showList();
                }
                render(page, next == null);
            });
    }

    private void render(JSONObject page, boolean first) throws JSONException {
        JSONArray containers = page.getJSONArray("containers");
        JSONArray folders = page.getJSONArray("folders");
        List<ArchiveList.Entry> entries = new ArrayList<>();
        Screen.FileKind kind = folder.equals("/") ? Screen.FileKind.DISK : Screen.FileKind.FOLDER;
        for (int i = 0; i < folders.length(); i++) {
            String child = folders.getJSONObject(i).getString("folder");
            String name = child.substring(child.lastIndexOf('/') + 1);
            entries.add(new ArchiveList.Entry(name, "", kind, () -> browse(child, null)));
        }
        for (int i = 0; i < containers.length(); i++) {
            entries.add(ArchiveList.file(containers.getJSONObject(i), this::open,
                file -> { if (!busy) uploads.fileMenu(folder, file); }));
        }
        String cursor = page.isNull("next_cursor") ? null : page.getString("next_cursor");
        files.append(entries, cursor == null ? null : () -> browse(folder, cursor));
        if (first && entries.isEmpty()) screen.showEmptyList(files);
    }

    private void browseBundle(BundleDirectory location, String next) {
        background(() -> location.browse(args(), next, documentCache()),
            page -> renderBundle(location, page, next == null));
    }

    private void renderBundle(BundleDirectory location, JSONObject page, boolean first) throws JSONException {
        BundleDirectory current = first ? location.atVersion(page.getLong("version")) : bundle;
        var rows = BundleDirectory.rows(page,
            path -> browseBundle(current.at(path), null), path -> openBundleMember(current, path));
        String cursor = page.isNull("next_cursor") ? null : page.getString("next_cursor");
        if (first) {
            bundle = current;
            showList();
        }
        files.append(rows, cursor == null ? null : () -> browseBundle(current, cursor));
        if (first && rows.isEmpty()) screen.showEmptyList(files);
    }

    void reloadList() {
        showList();
        if (bundle == null) browse(folder, null);
        else browseBundle(bundle, null);
    }

    private File documentCache() { return new File(getCacheDir(), DocumentProvider.CACHE); }

    private void open(ArchiveList.Container entry) {
        File cache = documentCache();
        Callable<JSONObject> request = () -> Bridge.call("open", args().put("id", entry.id())
            .put("version", entry.version()).put("cache", cache.getAbsolutePath()));
        background(request, data -> {
            if (data.optBoolean("bundle")) renderBundle(BundleDirectory.root(entry), data, true);
            else if (data.has("url")) play(FilePreview.Stream.from(data),
                ready -> background(() -> FilePreview.Stream.from(request.call()), ready::accept));
            else FilePreview.document(this, new File(data.getString("path")), data.getString("type"));
        });
    }

    private void openBundleMember(BundleDirectory location, String path) {
        background(() -> location.open(args(), path, documentCache()), data -> {
            File file = new File(data.getString("path"));
            String type = data.getString("type");
            if (type.startsWith("video/") || type.startsWith("audio/")) {
                play(new FilePreview.Stream(DocumentProvider.uri(file), Map.of()), null);
            }
            else FilePreview.document(this, file, type);
        });
    }

    private void play(FilePreview.Stream stream, FilePreview.Refresh refresh) {
        reset("原件播放", null);
        backAction("返回文件列表", () -> afterWork(() -> {
            if (video != null) reloadList();
        }));
        video = FilePreview.play(this, screen, stream, refresh);
    }

    private void logout() {
        background(() -> {
            try { return Bridge.call("logout", args()); }
            catch (IOException unavailable) { return new JSONObject(); }
        }, ignored -> {
            savedPassword = null;
            bundle = null;
            sessions.clear();
            showLogin();
        });
    }

    @Override protected void onActivityResult(int request, int result, Intent intent) {
        super.onActivityResult(request, result, intent);
        if (savedPassword == null) return;
        uploads.receive(request, result == RESULT_OK ? intent : null);
    }

    JSONObject args() throws JSONException {
        return new JSONObject().put("connection", new JSONObject().put("server", server).put("password", savedPassword));
    }

    void afterWork(Runnable action) {
        if (busy) worker.submit(() -> runOnUiThread(() -> { if (!isDestroyed()) action.run(); }));
        else action.run();
    }

    void uploadProgress(String name) { runOnUiThread(() -> { if (!isDestroyed()) screen.task(name); }); }

    interface Done<T> { void run(T value) throws Exception; }

    <T> void background(Callable<T> work, Done<T> done) {
        if (busy) return;
        busy = true;
        screen.busy(true);
        var progress = new Bridge.Progress() {
            @Override void update(int direction, long transferred, long total, long rate) {
                runOnUiThread(() -> {
                    if (!isDestroyed() && !cancelled()) screen.transfer(direction, transferred, total, rate, this::cancel);
                });
            }
        };
        worker.submit(() -> {
            try {
                T value = Bridge.observe(work, progress);
                runOnUiThread(() -> finish(progress, () -> done.run(value)));
            } catch (Exception error) {
                runOnUiThread(() -> finish(progress, () -> { throw error; }));
            }
        });
    }

    private void finish(Bridge.Progress progress, WorkDone done) {
        if (isDestroyed()) return;
        busy = false;
        screen.busy(false);
        if (progress.cancelled()) return;
        try { done.run(); } catch (Exception error) {
            if (error instanceof Bridge.Failure failure && failure.passwordRejected && savedPassword != null) {
                savedPassword = null;
                bundle = null;
                sessions.clear();
                showLogin();
            }
            String message = error.getMessage();
            screen.notice(message == null ? "操作失败" : message);
        }
    }

    private interface WorkDone { void run() throws Exception; }
}
