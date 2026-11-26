package local.zxs.mobile;

import org.json.JSONException;
import org.json.JSONObject;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.Callable;

final class Bridge {
    static { System.loadLibrary("zxs_jni"); }

    interface Progress { void update(int direction, long transferred, long total, long rate); }

    private static final ThreadLocal<Progress> PROGRESS = new ThreadLocal<>();
    private static native byte[] callNative(byte[] request, Progress progress);

    static <T> T observe(Callable<T> work, Progress progress) throws Exception {
        PROGRESS.set(progress);
        try { return work.call(); } finally { PROGRESS.remove(); }
    }

    static JSONObject call(String action, JSONObject args) throws IOException, JSONException {
        args.put("action", action);
        byte[] output = callNative(args.toString().getBytes(StandardCharsets.UTF_8), PROGRESS.get());
        if (output == null) throw new IOException("ZXS 没有返回结果");
        JSONObject reply = new JSONObject(new String(output, StandardCharsets.UTF_8));
        if (!reply.optBoolean("ok")) {
            throw new Failure(reply.optString("error", "操作失败"), reply.optInt("status"),
                reply.optBoolean("password_rejected"));
        }
        return reply.getJSONObject("data");
    }

    static final class Failure extends IOException {
        final int status;
        final boolean passwordRejected;

        Failure(String message, int status, boolean passwordRejected) {
            super(message);
            this.status = status;
            this.passwordRejected = passwordRejected;
        }
    }
}
