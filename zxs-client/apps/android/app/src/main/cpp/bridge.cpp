#include <cstring>
#include <limits>
#include <jni.h>
#include "zxs.h"

namespace {
struct Progress {
    JNIEnv *env;
    jobject receiver;
    jmethodID update;
    jmethodID cancelled;
};

void transfer(void *context, uint8_t direction, uint64_t transferred, uint64_t total, uint64_t rate) {
    const auto &progress = *static_cast<const Progress *>(context);
    if (!progress.env->ExceptionCheck()) {
        progress.env->CallVoidMethod(progress.receiver, progress.update, static_cast<jint>(direction),
            static_cast<jlong>(transferred), static_cast<jlong>(total), static_cast<jlong>(rate));
    }
}

bool cancelled(void *context) {
    const auto &progress = *static_cast<const Progress *>(context);
    return progress.env->ExceptionCheck()
        || progress.env->CallBooleanMethod(progress.receiver, progress.cancelled) == JNI_TRUE;
}
}

extern "C" JNIEXPORT jbyteArray JNICALL
Java_local_zxs_mobile_Bridge_callNative(JNIEnv *env, jclass, jbyteArray request, jobject observer) {
    if (request == nullptr) return nullptr;
    Progress progress{env, observer, nullptr, nullptr};
    if (observer != nullptr) {
        jclass type = env->GetObjectClass(observer);
        if (type == nullptr) return nullptr;
        progress.update = env->GetMethodID(type, "update", "(IJJJ)V");
        if (progress.update != nullptr) progress.cancelled = env->GetMethodID(type, "cancelled", "()Z");
        env->DeleteLocalRef(type);
        if (progress.cancelled == nullptr) return nullptr;
    }
    const jsize length = env->GetArrayLength(request);
    jbyte *bytes = env->GetByteArrayElements(request, nullptr);
    if (bytes == nullptr) return nullptr;
    char *result = zxs_call_with_transfer(reinterpret_cast<const uint8_t *>(bytes), length,
        observer == nullptr ? nullptr : transfer, observer == nullptr ? nullptr : cancelled, &progress);
    env->ReleaseByteArrayElements(request, bytes, JNI_ABORT);
    if (result == nullptr) return nullptr;
    if (env->ExceptionCheck()) { zxs_free(result); return nullptr; }
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
