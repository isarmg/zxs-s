package local.zxs.mobile;

import android.os.Bundle;
import java.io.File;
import java.io.IOException;
import java.util.ArrayList;
import java.util.List;
import java.util.function.Consumer;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

record BundleDirectory(String id, long version, String name, String path) {
    static BundleDirectory root(ArchiveList.Container entry) {
        return new BundleDirectory(entry.id(), 0, entry.name(), ".");
    }

    static BundleDirectory restore(Bundle state) {
        String id = state.getString("bundle_id");
        return id == null ? null : new BundleDirectory(id, state.getLong("bundle_version"),
            state.getString("bundle_name"), state.getString("bundle_directory", "."));
    }

    void save(Bundle state) {
        state.putString("bundle_id", id);
        state.putLong("bundle_version", version);
        state.putString("bundle_name", name);
        state.putString("bundle_directory", path);
    }

    BundleDirectory at(String directory) { return new BundleDirectory(id, version, name, directory); }

    BundleDirectory atVersion(long selected) { return new BundleDirectory(id, selected, name, path); }

    boolean isRoot() { return path.equals("."); }

    BundleDirectory parent() {
        int slash = path.lastIndexOf('/');
        return at(slash < 0 ? "." : path.substring(0, slash));
    }

    String displayPath(String remoteParent) {
        return remoteParent + "/" + name + (isRoot() ? "" : "/" + path);
    }

    JSONObject browse(JSONObject connection, String cursor, File cache) throws IOException, JSONException {
        return Bridge.call("browse_bundle", fields(connection, cache).put("directory", path).put("cursor", cursor));
    }

    JSONObject open(JSONObject connection, String member, File cache) throws IOException, JSONException {
        return Bridge.call("open_bundle_member", fields(connection, cache).put("path", member));
    }

    private JSONObject fields(JSONObject connection, File cache) throws JSONException {
        if (version != 0) connection.put("version", version);
        return connection.put("id", id).put("cache", cache.getAbsolutePath());
    }

    static List<ArchiveList.Entry> rows(JSONObject page, Consumer<String> browse, Consumer<String> open)
            throws JSONException {
        JSONArray entries = page.getJSONArray("entries");
        List<ArchiveList.Entry> rows = new ArrayList<>(entries.length());
        for (int i = 0; i < entries.length(); i++) {
            JSONObject entry = entries.getJSONObject(i);
            String path = entry.getString("path");
            String name = entry.getString("name");
            boolean directory = entry.getString("kind").equals("directory");
            rows.add(new ArchiveList.Entry(name, "",
                directory ? Screen.FileKind.FOLDER : Screen.FileKind.of(name),
                () -> (directory ? browse : open).accept(path)));
        }
        return rows;
    }
}
