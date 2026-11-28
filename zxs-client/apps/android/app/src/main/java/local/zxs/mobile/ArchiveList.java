package local.zxs.mobile;

import android.view.View;
import android.view.ViewGroup;
import android.widget.ArrayAdapter;
import android.widget.FrameLayout;
import android.widget.ListView;
import java.time.Instant;
import java.time.ZoneId;
import java.time.format.DateTimeFormatter;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.StringJoiner;
import java.util.function.Consumer;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import static android.view.ViewGroup.LayoutParams.MATCH_PARENT;
import static android.view.ViewGroup.LayoutParams.WRAP_CONTENT;

final class ArchiveList extends ListView {
    record Entry(String containerId, String name, String subtitle, Screen.FileKind kind, Runnable open, Runnable more) {
        Entry(String name, String subtitle, Screen.FileKind kind, Runnable open) {
            this(null, name, subtitle, kind, open, null);
        }
    }
    record Container(String id, long version, String name) {}

    private static final DateTimeFormatter TIME = DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm", Locale.ROOT);

    static Entry file(JSONObject entry, Consumer<Container> open, Consumer<Container> more) throws JSONException {
        String id = entry.getString("container_id");
        String name = entry.getString("name");
        var updated = Instant.ofEpochSecond(entry.getLong("updated_at")).atZone(ZoneId.systemDefault());
        long version = entry.getLong("latest_version");
        var subtitle = new StringJoiner(" · ").add("v" + version).add(TIME.format(updated));
        JSONArray tags = entry.getJSONArray("tags");
        for (int i = 0; i < tags.length(); i++) subtitle.add(tags.getString(i));
        var target = new Container(id, version, name);
        return new Entry(id, name, subtitle.toString(), Screen.FileKind.of(name),
            () -> open.accept(target), () -> more.accept(target));
    }

    private final List<Entry> entries = new ArrayList<>();
    private final ArrayAdapter<Entry> rows;
    private final FrameLayout footer;
    private Runnable nextPage;

    ArchiveList(Screen screen) {
        super(screen.root.getContext());
        rows = new ArrayAdapter<>(getContext(), android.R.layout.simple_list_item_1, entries) {
            @Override public View getView(int position, View recycled, ViewGroup parent) {
                var row = recycled instanceof Screen.FileRow ready ? ready : screen.new FileRow();
                row.bind(getItem(position));
                return row;
            }
        };
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
            if (getItemAtPosition(position) instanceof Entry entry) entry.open().run();
        });
    }

    void append(List<Entry> page, Runnable next) {
        nextPage = next;
        if (next != null && getFooterViewsCount() == 0) addFooterView(footer, null, false);
        if (next == null && getFooterViewsCount() != 0) removeFooterView(footer);
        var ids = new HashSet<String>();
        page.stream().map(Entry::containerId).filter(id -> id != null).forEach(ids::add);
        if (!ids.isEmpty()) entries.removeIf(entry -> ids.contains(entry.containerId()));
        rows.addAll(page);
    }

}
