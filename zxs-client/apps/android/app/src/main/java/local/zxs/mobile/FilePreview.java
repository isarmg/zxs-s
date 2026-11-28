package local.zxs.mobile;

import android.app.Activity;
import android.content.Intent;
import android.media.MediaPlayer;
import android.net.Uri;
import android.view.ViewGroup;
import android.widget.MediaController;
import android.widget.VideoView;
import java.io.File;
import java.util.Map;
import java.util.function.Consumer;
import org.json.JSONException;
import org.json.JSONObject;

final class FilePreview {
    private FilePreview() {}

    record Stream(Uri url, Map<String, String> headers) {
        static Stream from(JSONObject data) throws JSONException {
            return new Stream(Uri.parse(data.getString("url")),
                Map.of("Authorization", data.getJSONObject("headers").getString("Authorization")));
        }
    }

    interface Refresh { void request(Consumer<Stream> ready); }

    static VideoView play(Activity activity, Screen screen, Stream stream, Refresh refresh) {
        VideoView video = new VideoView(activity);
        screen.scrollContent().addView(video, ViewGroup.LayoutParams.MATCH_PARENT, screen.dp(360));
        video.setMediaController(new MediaController(activity));
        start(video, screen, stream, refresh, 0);
        return video;
    }

    private static void start(VideoView video, Screen screen, Stream stream, Refresh refresh, int position) {
        video.setOnErrorListener((player, error, extra) -> {
            if (refresh == null) {
                screen.notice("系统播放器无法播放此文件。");
            } else {
                int resume = position(player);
                refresh.request(updated -> {
                    if (!video.isAttachedToWindow()) return;
                    if (updated.equals(stream)) screen.notice("系统播放器无法播放此文件。");
                    else start(video, screen, updated, refresh, resume);
                });
            }
            return true;
        });
        video.setVideoURI(stream.url(), stream.headers());
        if (position > 0) video.seekTo(position);
        video.start();
    }

    private static int position(MediaPlayer player) {
        try { return Math.max(0, player.getCurrentPosition()); }
        catch (IllegalStateException failedPlayback) { return 0; }
    }

    static void document(Activity activity, File file, String type) {
        Uri uri = DocumentProvider.uri(file);
        Intent intent = new Intent(Intent.ACTION_VIEW);
        String mime = type.equals("application/octet-stream") ? activity.getContentResolver().getType(uri) : type;
        intent.setDataAndTypeAndNormalize(uri, mime);
        intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        activity.startActivity(Intent.createChooser(intent, "查看文件"));
    }
}
