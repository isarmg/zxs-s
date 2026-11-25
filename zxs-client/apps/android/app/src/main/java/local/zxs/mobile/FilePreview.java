package local.zxs.mobile;

import android.app.Activity;
import android.content.Intent;
import android.net.Uri;
import android.view.ViewGroup;
import android.widget.LinearLayout;
import android.widget.MediaController;
import android.widget.VideoView;
import java.io.File;
import java.util.Map;

final class FilePreview {
    private FilePreview() {}

    static VideoView play(Activity activity, Screen screen, Uri url, Map<String, String> headers) {
        VideoView video = new VideoView(activity);
        video.setLayoutParams(new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, screen.dp(360)));
        screen.scrollContent().addView(video);
        MediaController controls = new MediaController(activity);
        controls.setAnchorView(video);
        video.setMediaController(controls);
        video.setOnErrorListener((player, error, extra) -> {
            screen.notice("系统播放器无法播放此文件。", true);
            return true;
        });
        video.setVideoURI(url, headers);
        video.start();
        return video;
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
