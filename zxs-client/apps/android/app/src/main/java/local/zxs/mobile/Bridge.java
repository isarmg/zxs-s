package local.zxs.mobile;

import org.json.JSONException;
import org.json.JSONObject;
import java.io.IOException;
import java.nio.charset.StandardCharsets;

final class Bridge {
    static { System.loadLibrary("zxs_jni"); }

    private static native byte[] callNative(byte[] request);

    static JSONObject call(String action, JSONObject args) throws IOException, JSONException {
        args.put("action", action);
        byte[] output = callNative(args.toString().getBytes(StandardCharsets.UTF_8));
        if (output == null) throw new IOException("ZXS 没有返回结果");
        JSONObject reply = new JSONObject(new String(output, StandardCharsets.UTF_8));
        if (!reply.optBoolean("ok")) {
            throw new Failure(reply.optString("error", "操作失败"), reply.optBoolean("password_rejected"));
        }
        return reply.getJSONObject("data");
    }

    static final class Failure extends IOException {
        final boolean passwordRejected;

        Failure(String message, boolean passwordRejected) {
            super(message);
            this.passwordRejected = passwordRejected;
        }
    }
}
