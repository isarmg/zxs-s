package local.zxs.mobile;

import android.content.Context;
import android.content.SharedPreferences;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.util.Base64;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.security.GeneralSecurityException;
import java.security.KeyStore;
import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

final class SessionStore {
    private static final String PROVIDER = "AndroidKeyStore";
    private static final String ALIAS = "zxs.credentials";
    private final SharedPreferences data;

    SessionStore(Context context) {
        data = context.getSharedPreferences("credentials", Context.MODE_PRIVATE);
    }

    record Account(String server, String password) {}

    Account load() throws GeneralSecurityException, IOException {
        String encrypted = data.getString("encrypted", "");
        if (encrypted.isEmpty()) return new Account("", "");
        byte[] iv = Base64.decode(data.getString("iv", ""), Base64.NO_WRAP);
        Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
        cipher.init(Cipher.DECRYPT_MODE, key(false), new GCMParameterSpec(128, iv));
        String credential = new String(cipher.doFinal(Base64.decode(encrypted, Base64.NO_WRAP)), StandardCharsets.UTF_8);
        int separator = credential.indexOf('\n');
        if (separator < 1) throw new IOException("登录凭据损坏");
        return new Account(credential.substring(0, separator), credential.substring(separator + 1));
    }

    void save(String server, String password) throws GeneralSecurityException, IOException {
        Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
        cipher.init(Cipher.ENCRYPT_MODE, key(true));
        byte[] encrypted = cipher.doFinal((server + "\n" + password).getBytes(StandardCharsets.UTF_8));
        boolean saved = data.edit().clear()
            .putString("encrypted", Base64.encodeToString(encrypted, Base64.NO_WRAP))
            .putString("iv", Base64.encodeToString(cipher.getIV(), Base64.NO_WRAP))
            .commit();
        if (!saved) throw new IOException("无法保存登录凭据");
    }

    void clear() { data.edit().clear().apply(); }

    private static SecretKey key(boolean create) throws GeneralSecurityException, IOException {
        KeyStore keys = KeyStore.getInstance(PROVIDER);
        keys.load(null);
        if (keys.getKey(ALIAS, null) instanceof SecretKey key) return key;
        if (!create) throw new GeneralSecurityException("登录密钥不可用");
        KeyGenerator generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, PROVIDER);
        generator.init(new KeyGenParameterSpec.Builder(ALIAS,
            KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
            .setKeySize(256)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .build());
        return generator.generateKey();
    }
}
