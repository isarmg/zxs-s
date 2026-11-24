package local.zxs.mobile;

import android.app.Activity;
import android.content.ClipData;
import android.content.Intent;
import android.content.SharedPreferences;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.provider.MediaStore;
import android.provider.OpenableColumns;
import android.text.InputType;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.MediaController;
import android.widget.ProgressBar;
import android.widget.ScrollView;
import android.widget.TextView;
import android.widget.VideoView;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import org.json.JSONArray;
import org.json.JSONObject;

public final class MainActivity extends Activity {
    private static final int PHOTO_PICKER = 1;
    private static final String ALBUM = "/相册";
    private final ExecutorService worker = Executors.newSingleThreadExecutor();
    private SharedPreferences preferences;
    private String server;
    private String token;
    private String folder = "/";
    private LinearLayout content;
    private TextView message;
    private ProgressBar progress;
    private VideoView video;
    private boolean busy;

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        preferences = getSharedPreferences("session", MODE_PRIVATE);
        server = preferences.getString("server", "");
        token = preferences.getString("token", "");
        if (token.isEmpty()) showLogin();
        else browse("/", null);
    }

    @Override protected void onDestroy() {
        if (video != null) video.stopPlayback();
        worker.shutdownNow();
        super.onDestroy();
    }

    private void reset(String title) {
        if (video != null) {
            video.stopPlayback();
            video = null;
        }
        ScrollView scroll = new ScrollView(this);
        content = new LinearLayout(this);
        content.setOrientation(LinearLayout.VERTICAL);
        int pad = dp(16);
        content.setPadding(pad, pad, pad, pad);
        scroll.addView(content);
        scroll.setOnApplyWindowInsetsListener((view, insets) -> {
            var bars = insets.getInsets(WindowInsets.Type.systemBars());
            view.setPadding(bars.left, bars.top, bars.right, bars.bottom);
            return insets;
        });
        setContentView(scroll);
        TextView heading = new TextView(this);
        heading.setText(title);
        heading.setTextSize(24);
        content.addView(heading);
        message = new TextView(this);
        message.setAccessibilityLiveRegion(TextView.ACCESSIBILITY_LIVE_REGION_POLITE);
        content.addView(message);
        progress = new ProgressBar(this);
        progress.setVisibility(ProgressBar.GONE);
        content.addView(progress);
    }

    private void showLogin() {
        reset("ZXS");
        EditText endpoint = new EditText(this);
        endpoint.setHint("局域网服务器地址");
        endpoint.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI);
        endpoint.setText(server.isEmpty() ? "http://" : server);
        content.addView(endpoint);
        EditText password = new EditText(this);
        password.setHint("口令");
        password.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD);
        content.addView(password);
        button("连接", () -> {
            String address = endpoint.getText().toString();
            String secret = password.getText().toString();
            background(() -> Bridge.call("login", new JSONObject().put("server", address).put("password", secret)),
                result -> {
                    server = result.getString("server");
                    token = result.getJSONObject("session").getString("token");
                    preferences.edit().putString("server", server).putString("token", token).apply();
                    password.setText("");
                    browse("/", null);
                });
        });
    }

    private void browse(String requested, String next) {
        if (next == null) {
            folder = requested;
            reset("ZXS   " + folder);
            button("选择相片和视频备份", this::pickPhotos);
            button("刷新", () -> browse(folder, null));
            button("退出", this::logout);
            if (!folder.equals("/")) {
                int slash = folder.lastIndexOf('/');
                button("上一级", () -> browse(slash == 0 ? "/" : folder.substring(0, slash), null));
            }
        }
        background(() -> Bridge.call("browse", args().put("folder", requested).put("cursor", next)),
            page -> render(page, next == null));
    }

    private void render(JSONObject page, boolean first) throws Exception {
        JSONArray files = page.getJSONArray("containers");
        JSONArray folders = page.getJSONArray("folders");
        if (first) {
            for (int i = 0; i < folders.length(); i++) {
                String child = folders.getJSONObject(i).getString("folder");
                button("▰  " + child.substring(child.lastIndexOf('/') + 1), () -> browse(child, null));
            }
        }
        for (int i = 0; i < files.length(); i++) {
            JSONObject entry = files.getJSONObject(i);
            String id = entry.getString("container_id");
            button(entry.getString("name"), () -> open(id));
        }
        if (!page.isNull("next_cursor")) {
            String cursor = page.getString("next_cursor");
            Button more = button("加载下一页", () -> {});
            more.setOnClickListener(view -> {
                if (busy) return;
                content.removeView(view);
                browse(folder, cursor);
            });
        }
        if (first && folders.length() == 0 && files.length() == 0) message.setText("此目录为空");
    }

    private void open(String id) {
        background(() -> Bridge.call("media", args().put("id", id)), media -> {
            String type = media.getString("type");
            if (type.startsWith("video/") || type.startsWith("audio/")) play(media.getString("url"));
            else download(id);
        });
    }

    private void play(String url) {
        reset("原件播放");
        button("返回", () -> browse(folder, null));
        video = new VideoView(this);
        video.setLayoutParams(new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(360)));
        content.addView(video);
        MediaController controls = new MediaController(this);
        controls.setAnchorView(video);
        video.setMediaController(controls);
        video.setOnErrorListener((player, error, extra) -> {
            message.setText("系统播放器无法播放此文件。");
            return true;
        });
        video.setVideoURI(Uri.parse(url));
        video.start();
    }

    private void download(String id) {
        File cache = new File(getCacheDir(), DocumentProvider.CACHE);
        background(() -> Bridge.call("document", args().put("id", id).put("cache", cache.getAbsolutePath())), data -> {
            Uri uri = DocumentProvider.uri(new File(data.getString("path")));
            Intent intent = new Intent(Intent.ACTION_VIEW);
            intent.setDataAndType(uri, getContentResolver().getType(uri));
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
            startActivity(Intent.createChooser(intent, "查看文件"));
        });
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
            preferences.edit().remove("token").apply();
            showLogin();
        });
    }

    private void pickPhotos() {
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
        }, count -> {
            browse(ALBUM, null);
            message.setText("已检查并备份 " + count + " 个项目");
        });
    }

    private void backup(Uri uri) throws Exception {
        String name = "media";
        try (Cursor cursor = getContentResolver().query(uri, new String[] { OpenableColumns.DISPLAY_NAME }, null, null, null)) {
            if (cursor != null && cursor.moveToFirst() && cursor.getString(0) != null) name = cursor.getString(0);
        }
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

    private JSONObject args() throws Exception {
        return new JSONObject().put("connection", new JSONObject().put("server", server).put("token", token));
    }

    private int dp(int value) { return (int) (getResources().getDisplayMetrics().density * value); }

    private Button button(String text, Runnable action) {
        Button button = new Button(this);
        button.setText(text);
        button.setAllCaps(false);
        button.setOnClickListener(view -> { if (!busy) action.run(); });
        content.addView(button);
        return button;
    }

    private interface Work<T> { T run() throws Exception; }
    private interface Done<T> { void run(T value) throws Exception; }

    private <T> void background(Work<T> work, Done<T> done) {
        if (busy) return;
        busy = true;
        progress.setVisibility(ProgressBar.VISIBLE);
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
        progress.setVisibility(ProgressBar.GONE);
        try { done.run(); } catch (Exception error) { message.setText(error.getMessage()); }
    }

    private interface WorkDone { void run() throws Exception; }
}
