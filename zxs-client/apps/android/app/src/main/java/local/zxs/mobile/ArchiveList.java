package local.zxs.mobile;

import android.view.View;
import android.view.ViewGroup;
import android.widget.BaseAdapter;
import android.widget.FrameLayout;
import android.widget.ListView;
import java.util.ArrayList;
import java.util.List;

import static android.view.ViewGroup.LayoutParams.MATCH_PARENT;
import static android.view.ViewGroup.LayoutParams.WRAP_CONTENT;

final class ArchiveList extends ListView {
    record Entry(String name, String subtitle, Screen.FileKind kind, Runnable open) {}

    private final List<Entry> entries = new ArrayList<>();
    private final Screen screen;
    private final Rows rows = new Rows();
    private final FrameLayout footer;
    private Runnable nextPage;

    ArchiveList(Screen screen) {
        super(screen.root.getContext());
        this.screen = screen;
        setDivider(null);
        setDrawSelectorOnTop(true);
        setSelector(screen.ripple(android.R.color.transparent, 0, false));
        setClipToPadding(false);
        footer = new FrameLayout(getContext());
        int margin = screen.dp(16);
        footer.setPadding(margin, screen.dp(8), margin, 0);
        var button = screen.button("加载下一页", false, () -> {
            if (nextPage != null) nextPage.run();
        });
        footer.addView(button, MATCH_PARENT, WRAP_CONTENT);
        setAdapter(rows);
        setOnItemClickListener((parent, row, position, id) -> {
            if (position < entries.size()) entries.get(position).open().run();
        });
    }

    void append(List<Entry> page, Runnable next) {
        entries.addAll(page);
        nextPage = next;
        if (next != null && getFooterViewsCount() == 0) addFooterView(footer, null, false);
        if (next == null && getFooterViewsCount() != 0) removeFooterView(footer);
        rows.notifyDataSetChanged();
    }

    private final class Rows extends BaseAdapter {
        @Override public int getCount() { return entries.size(); }
        @Override public Entry getItem(int position) { return entries.get(position); }
        @Override public long getItemId(int position) { return position; }

        @Override public View getView(int position, View recycled, ViewGroup parent) {
            var row = recycled instanceof Screen.FileRow ready ? ready : screen.new FileRow();
            row.bind(getItem(position));
            return row;
        }
    }
}
