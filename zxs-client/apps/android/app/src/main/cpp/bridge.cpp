#include <cstring>
#include <limits>
#include <jni.h>
#include "zxs.h"

extern "C" JNIEXPORT jbyteArray JNICALL
Java_local_zxs_mobile_Bridge_callNative(JNIEnv *env, jclass, jbyteArray request) {
    if (request == nullptr) return nullptr;
    const jsize length = env->GetArrayLength(request);
    jbyte *bytes = env->GetByteArrayElements(request, nullptr);
    if (bytes == nullptr) return nullptr;
    char *result = zxs_call(reinterpret_cast<const uint8_t *>(bytes), length);
    env->ReleaseByteArrayElements(request, bytes, JNI_ABORT);
    if (result == nullptr) return nullptr;
    const size_t size = std::strlen(result);
    jbyteArray response = nullptr;
    if (size <= static_cast<size_t>(std::numeric_limits<jsize>::max())) {
        const auto count = static_cast<jsize>(size);
        response = env->NewByteArray(count);
        if (response != nullptr) {
            env->SetByteArrayRegion(response, 0, count, reinterpret_cast<const jbyte *>(result));
        }
    }
    zxs_free(result);
    return response;
}
