package local.zxs.mobile;

import android.content.Context;
import android.content.res.ColorStateList;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.graphics.drawable.RippleDrawable;
import android.text.TextUtils;
import android.view.Gravity;
import android.view.View;
import android.view.WindowInsets;
import android.webkit.MimeTypeMap;
import android.widget.Button;
import android.widget.FrameLayout;
import android.widget.ImageButton;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.ProgressBar;
import android.widget.ScrollView;
import android.widget.TextView;
import java.util.Locale;

import static android.view.ViewGroup.LayoutParams.MATCH_PARENT;
import static android.view.ViewGroup.LayoutParams.WRAP_CONTENT;

final class Screen {
    private static final int GUTTER = 16;
    private static final int FAB_SIZE = 56;
    private final Context context;
    final FrameLayout root;
    private final FrameLayout body;
    private final LinearLayout bar;
    private final ProgressBar progress;
    private final TextView task;
    private final TextView transfer;
    private View floatingAction;
    private View notice;

    Screen(Context context, String title, String path) {
        this.context = context;
        root = new FrameLayout(context);
        root.setBackgroundColor(context.getColor(R.color.zxs_canvas));
        root.setOnApplyWindowInsetsListener((view, insets) -> {
            var safe = insets.getInsets(WindowInsets.Type.systemBars()
                | WindowInsets.Type.displayCutout() | WindowInsets.Type.ime());
            view.setPadding(safe.left, safe.top, safe.right, safe.bottom);
            return insets;
        });
        var shell = column();
        root.addView(shell, new FrameLayout.LayoutParams(MATCH_PARENT, MATCH_PARENT));

        bar = new LinearLayout(context);
        bar.setGravity(Gravity.CENTER_VERTICAL);
        bar.setPadding(dp(8), 0, dp(8), 0);
        bar.setBackgroundColor(context.getColor(R.color.zxs_surface));
        shell.addView(bar, MATCH_PARENT, dp(56));
        var heading = column();
        heading.setPadding(dp(8), 0, dp(8), 0);
        bar.addView(heading, new LinearLayout.LayoutParams(0, WRAP_CONTENT, 1));
        TextView headingText = label(title, 20, 600, R.color.zxs_primary);
        headingText.setAccessibilityHeading(true);
        heading.addView(headingText);
        if (path != null) heading.addView(label(path, 12, 500, R.color.zxs_secondary));
        shell.addView(line(R.color.zxs_border), MATCH_PARENT, dp(1));

        progress = new ProgressBar(context, null, android.R.attr.progressBarStyleHorizontal);
        progress.setIndeterminate(true);
        progress.setIndeterminateTintList(tint(R.color.zxs_accent_text));
        progress.setProgressTintList(tint(R.color.zxs_accent_text));
        shell.addView(progress, MATCH_PARENT, dp(4));
        task = label("", 13, 500, R.color.zxs_secondary);
        task.setPadding(dp(GUTTER), dp(4), dp(GUTTER), dp(4));
        shell.addView(task, MATCH_PARENT, WRAP_CONTENT);
        transfer = label("", 13, 500, R.color.zxs_secondary);
        transfer.setPadding(dp(GUTTER), dp(4), dp(GUTTER), dp(4));
        shell.addView(transfer, MATCH_PARENT, WRAP_CONTENT);
        busy(false);

        body = new FrameLayout(context);
        shell.addView(body, new LinearLayout.LayoutParams(MATCH_PARENT, 0, 1));
    }

    LinearLayout scrollContent() {
        var scroll = new ScrollView(context);
        body.addView(scroll, MATCH_PARENT, MATCH_PARENT);
        var content = column();
        scroll.addView(content, MATCH_PARENT, WRAP_CONTENT);
        return content;
    }

    void back(String title, Runnable action) {
        bar.addView(iconButton(R.drawable.ic_back, title, action), 0, new LinearLayout.LayoutParams(dp(48), dp(48)));
    }

    void command(int icon, String title, Runnable action) {
        bar.addView(iconButton(icon, title, action), dp(48), dp(48));
    }

    void upload(String title, Runnable action) {
        var fab = iconButton(R.drawable.ic_upload, title, action);
        fab.setBackground(ripple(R.color.zxs_accent, 12, true));
        fab.setImageTintList(tint(R.color.zxs_on_accent));
        fab.setPadding(dp(16), dp(16), dp(16), dp(16));
        var bounds = new FrameLayout.LayoutParams(dp(FAB_SIZE), dp(FAB_SIZE), Gravity.BOTTOM | Gravity.END);
        bounds.setMargins(dp(GUTTER), 0, dp(GUTTER), dp(GUTTER));
        root.addView(fab, bounds);
        floatingAction = fab;
    }

    ArchiveList fileList() {
        var list = new ArchiveList(this);
        list.setPadding(0, 0, 0, dp(FAB_SIZE + GUTTER + 8));
        body.addView(list, MATCH_PARENT, MATCH_PARENT);
        return list;
    }

    void addPrimaryButton(LinearLayout content, String title, Runnable action) {
        var bounds = new LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT);
        bounds.topMargin = dp(8);
        content.addView(button(title, true, action), bounds);
    }

    Button button(String title, boolean primary, Runnable action) {
        var button = new Button(context);
        button.setText(title);
        button.setAllCaps(false);
        button.setTextSize(14);
        button.setTypeface(Typeface.create(Typeface.DEFAULT, primary ? 600 : 500, false));
        button.setTextColor(context.getColor(primary ? R.color.zxs_on_accent : R.color.zxs_primary));
        button.setBackground(ripple(primary ? R.color.zxs_accent : R.color.zxs_subtle, 6, primary));
        button.setMinHeight(dp(44));
        button.setMinimumHeight(dp(44));
        button.setOnClickListener(view -> action.run());
        return button;
    }

    void showEmptyList(ArchiveList list) {
        TextView hint = label("此目录为空", 13, 400, R.color.zxs_muted);
        hint.setPadding(dp(GUTTER), dp(24), dp(GUTTER), dp(24));
        body.addView(hint, MATCH_PARENT, WRAP_CONTENT);
        list.setEmptyView(hint);
    }

    void busy(boolean active) {
        progress.setVisibility(active ? View.VISIBLE : View.INVISIBLE);
        if (!active) {
            progress.setIndeterminate(true);
            task.setVisibility(View.GONE);
            transfer.setVisibility(View.GONE);
        }
    }

    void backupProgress(int done, int total) {
        task("相册上传 " + done + " / " + total);
        progress.setIndeterminate(false);
        progress.setMax(total);
        progress.setProgress(done);
    }

    void task(String name) {
        task.setText(name);
        task.setVisibility(View.VISIBLE);
        progress.setIndeterminate(true);
        transfer.setVisibility(View.GONE);
    }

    void transfer(int direction, long transferred, long total, long rate) {
        int percent = total == 0 ? 100 : (int) Math.min(100, 100.0 * transferred / total);
        progress.setIndeterminate(false);
        progress.setMax(100);
        progress.setProgress(percent);
        String speed = android.text.format.Formatter.formatShortFileSize(context, rate);
        transfer.setText((direction == 0 ? "上传 " : "下载 ") + percent + "% · " + speed + "/s");
        transfer.setVisibility(View.VISIBLE);
    }

    void notice(String text, boolean error) {
        if (notice != null) root.removeView(notice);
        var toast = new LinearLayout(context);
        toast.setGravity(Gravity.CENTER_VERTICAL);
        toast.setPadding(dp(12), dp(4), dp(4), dp(4));
        GradientDrawable surface = shape(R.color.zxs_elevated, 12);
        surface.setStroke(dp(1), context.getColor(R.color.zxs_border));
        toast.setBackground(surface);
        ImageView icon = glyph(error ? R.drawable.ic_error : R.drawable.ic_check,
            error ? R.color.zxs_danger : R.color.zxs_ok);
        toast.addView(icon, dp(16), dp(16));
        TextView message = label(text, 13, 400, R.color.zxs_primary);
        message.setSingleLine(false);
        message.setPadding(dp(8), dp(4), dp(4), dp(4));
        message.setAccessibilityLiveRegion(View.ACCESSIBILITY_LIVE_REGION_POLITE);
        toast.addView(message, new LinearLayout.LayoutParams(0, WRAP_CONTENT, 1));
        toast.addView(iconButton(R.drawable.ic_close, "关闭提示", () -> root.removeView(toast)), dp(48), dp(48));
        var bounds = new FrameLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT, Gravity.BOTTOM);
        int bottom = floatingAction == null ? GUTTER : FAB_SIZE + GUTTER + 8;
        bounds.setMargins(dp(GUTTER), 0, dp(GUTTER), dp(bottom));
        root.addView(toast, bounds);
        notice = toast;
        if (!error) toast.postDelayed(() -> root.removeView(toast), 3500);
    }

    int dp(int value) {
        return Math.round(context.getResources().getDisplayMetrics().density * value);
    }

    private LinearLayout column() {
        var layout = new LinearLayout(context);
        layout.setOrientation(LinearLayout.VERTICAL);
        return layout;
    }

    private TextView label(String text, int size, int weight, int tone) {
        var label = new TextView(context);
        label.setText(text);
        label.setTextColor(context.getColor(tone));
        label.setTextSize(size);
        label.setTypeface(Typeface.create(Typeface.DEFAULT, weight, false));
        label.setSingleLine(true);
        label.setEllipsize(TextUtils.TruncateAt.END);
        return label;
    }

    private View line(int tone) {
        var line = new View(context);
        line.setBackgroundColor(context.getColor(tone));
        return line;
    }

    private ImageView glyph(int icon, int tone) {
        var view = new ImageView(context);
        view.setImageResource(icon);
        view.setImageTintList(tint(tone));
        view.setScaleType(ImageView.ScaleType.FIT_CENTER);
        view.setImportantForAccessibility(View.IMPORTANT_FOR_ACCESSIBILITY_NO);
        return view;
    }

    private ImageButton iconButton(int icon, String title, Runnable action) {
        var button = new ImageButton(context);
        button.setImageResource(icon);
        button.setImageTintList(tint(R.color.zxs_secondary));
        button.setScaleType(ImageView.ScaleType.FIT_CENTER);
        button.setPadding(dp(12), dp(12), dp(12), dp(12));
        button.setBackground(ripple(android.R.color.transparent, 6, false));
        button.setContentDescription(title);
        button.setTooltipText(title);
        button.setOnClickListener(view -> action.run());
        return button;
    }

    private GradientDrawable shape(int tone, int radius) {
        var shape = new GradientDrawable();
        shape.setColor(context.getColor(tone));
        shape.setCornerRadius(dp(radius));
        return shape;
    }

    RippleDrawable ripple(int tone, int radius, boolean primary) {
        return new RippleDrawable(tint(primary ? R.color.zxs_on_accent_ripple : R.color.zxs_ripple),
            shape(tone, radius), shape(R.color.zxs_primary, radius));
    }

    private ColorStateList tint(int tone) {
        return ColorStateList.valueOf(context.getColor(tone));
    }

    final class FileRow extends FrameLayout {
        private final ImageView icon;
        private final TextView name;
        private final TextView meta;
        private final ImageButton more;

        FileRow() {
            super(context);
            setMinimumHeight(dp(56));
            setBackgroundColor(context.getColor(R.color.zxs_surface));
            var row = new LinearLayout(context);
            row.setGravity(Gravity.CENTER_VERTICAL);
            row.setPadding(dp(GUTTER), dp(4), dp(GUTTER), dp(4));
            addView(row, new FrameLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT, Gravity.CENTER_VERTICAL));
            icon = glyph(R.drawable.ic_document, R.color.zxs_secondary);
            icon.setPadding(dp(8), dp(8), dp(8), dp(8));
            row.addView(icon, dp(36), dp(36));

            var description = column();
            var bounds = new LinearLayout.LayoutParams(0, WRAP_CONTENT, 1);
            bounds.setMarginStart(dp(12));
            row.addView(description, bounds);
            name = label("", 15, 500, R.color.zxs_primary);
            meta = label("", 12, 400, R.color.zxs_secondary);
            meta.setFontFeatureSettings("tnum");
            description.addView(name);
            description.addView(meta);
            more = iconButton(android.R.drawable.ic_menu_more, "更多", () -> {});
            more.setFocusable(false);
            row.addView(more, dp(44), dp(44));
            addView(line(R.color.zxs_divider),
                new FrameLayout.LayoutParams(MATCH_PARENT, dp(1), Gravity.BOTTOM));
        }

        void bind(ArchiveList.Entry entry) {
            name.setText(entry.name());
            meta.setText(entry.subtitle());
            FileKind kind = entry.kind();
            icon.setImageResource(kind.icon);
            icon.setImageTintList(tint(kind.ink));
            icon.setBackground(shape(kind.fill, 6));
            more.setVisibility(entry.more() == null ? GONE : VISIBLE);
            more.setContentDescription("更多 · " + entry.name());
            more.setOnClickListener(entry.more() == null ? null : view -> entry.more().run());
        }
    }

    enum FileKind {
        DISK(R.drawable.ic_disk, R.color.zxs_secondary, R.color.zxs_subtle),
        FOLDER(R.drawable.ic_folder, R.color.zxs_folder, R.color.zxs_folder_soft),
        DOCUMENT(R.drawable.ic_document, R.color.zxs_secondary, R.color.zxs_subtle),
        MEDIA(R.drawable.ic_media, R.color.zxs_media, R.color.zxs_media_soft),
        ARCHIVE(R.drawable.ic_archive, R.color.zxs_archive, R.color.zxs_archive_soft);

        final int icon;
        final int ink;
        final int fill;

        FileKind(int icon, int ink, int fill) {
            this.icon = icon;
            this.ink = ink;
            this.fill = fill;
        }

        static FileKind of(String name) {
            int dot = name.lastIndexOf('.');
            if (dot < 0) return DOCUMENT;
            String extension = name.substring(dot + 1).toLowerCase(Locale.ROOT);
            if (extension.equals("zip")) return ARCHIVE;
            String mime = MimeTypeMap.getSingleton().getMimeTypeFromExtension(extension);
            if (mime == null) return DOCUMENT;
            return mime.startsWith("image/") || mime.startsWith("video/") || mime.startsWith("audio/")
                ? MEDIA : DOCUMENT;
        }
    }
}
