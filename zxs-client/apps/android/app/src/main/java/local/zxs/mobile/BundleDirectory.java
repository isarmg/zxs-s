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
        return new BundleDirectory(entry.id(), entry.version(), entry.name(), ".");
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

    boolean isRoot() { return path.equals("."); }

    BundleDirectory parent() {
        int slash = path.lastIndexOf('/');
        return at(slash < 0 ? "." : path.substring(0, slash));
    }

    String displayPath(String remoteParent) {
        return remoteParent + "/" + name + (isRoot() ? "" : "/" + path);
    }

    Page browse(JSONObject connection, String cursor) throws IOException, JSONException {
        JSONObject reply = Bridge.call("browse_bundle", fields(connection).put("directory", path).put("cursor", cursor));
        List<Member> children = new ArrayList<>();
        JSONArray entries = reply.getJSONArray("entries");
        for (int i = 0; i < entries.length(); i++) {
            JSONObject entry = entries.getJSONObject(i);
            children.add(new Member(entry.getString("path"), entry.getString("name"),
                entry.getString("kind").equals("directory")));
        }
        return new Page(children, reply.isNull("next_cursor") ? null : reply.getString("next_cursor"));
    }

    JSONObject open(JSONObject connection, String member, File cache) throws IOException, JSONException {
        return Bridge.call("open_bundle_member", fields(connection).put("path", member).put("cache", cache.getAbsolutePath()));
    }

    private JSONObject fields(JSONObject connection) throws JSONException {
        return connection.put("id", id).put("version", version);
    }

    private record Member(String path, String name, boolean directory) {}

    record Page(List<Member> members, String cursor) {
        List<ArchiveList.Entry> rows(Consumer<String> browse, Consumer<String> open) {
            List<ArchiveList.Entry> rows = new ArrayList<>(members.size());
            for (Member entry : members) {
                rows.add(new ArchiveList.Entry(entry.name(), "",
                    entry.directory() ? Screen.FileKind.FOLDER : Screen.FileKind.of(entry.name()),
                    () -> (entry.directory() ? browse : open).accept(entry.path())));
            }
            return rows;
        }
    }
}
