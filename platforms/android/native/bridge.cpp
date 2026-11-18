#include <jni.h>
#include <algorithm>
#include <unistd.h>
#include <cstdlib>
#include <cstring>
#include <cstdint>
#include <mutex>
#include <string>
#include <vector>

extern "C" {
using SocketCallback = int (*)(int, const char *, const char *, const char *);
void avian_android_set_socket_callback(SocketCallback callback);
int avian_android_start_verified(const char *configuration, const unsigned char *credential, size_t length);
int avian_android_refresh_trust();
int avian_android_wait_instance_ready();
int avian_android_set_policy(const char *document);
const char *avian_android_update_underlays(const char *document);
int avian_android_reconcile_underlays(const char *document);
void avian_android_stop_verified();
int set_tun_fd(const char *instance, int descriptor);
const char *avian_android_publish_services(const char *document, const unsigned char *identity, size_t length);
const char *avian_android_service_page(const char *after, size_t limit);
const char *avian_android_event_page(uint64_t after, size_t limit);
const char *avian_android_status();
const char *avian_android_provision(uint8_t operation, const char *directory, const char *document, const char *fingerprint, const unsigned char *identity, size_t identity_length, const unsigned char *transport, size_t transport_length);
void free_string(const char *value);
}

namespace {
struct WipeMemory {
    void *address;
    size_t length;
    ~WipeMemory() {
        volatile unsigned char *bytes = static_cast<volatile unsigned char *>(address);
        for (size_t index = 0; index < length; ++index) bytes[index] = 0;
    }
};

JavaVM *virtual_machine = nullptr;
std::mutex callback_lock;
std::mutex lifecycle_lock;
uint64_t active_session = 0;
int active_tun_descriptor = -1;
uint64_t next_session = 0;
jobject socket_host = nullptr;

int prepare_socket(int descriptor, const char *destination, const char *source, const char *interface_name) {
    JNIEnv *environment = nullptr;
    bool attached = virtual_machine->GetEnv(reinterpret_cast<void **>(&environment), JNI_VERSION_1_6) != JNI_OK;
    if (attached && virtual_machine->AttachCurrentThread(&environment, nullptr) != JNI_OK) return -1;
    jobject host;
    {
        std::lock_guard<std::mutex> guard(callback_lock);
        host = socket_host == nullptr ? nullptr : environment->NewLocalRef(socket_host);
    }
    int result = -1;
    if (host != nullptr) {
        jclass host_class = environment->GetObjectClass(host);
        jmethodID method = environment->GetMethodID(host_class, "prepareSocket", "(ILjava/lang/String;Ljava/lang/String;Ljava/lang/String;)Z");
        if (method == nullptr) {
            environment->ExceptionClear();
            environment->DeleteLocalRef(host_class);
            environment->DeleteLocalRef(host);
            if (attached) virtual_machine->DetachCurrentThread();
            return -1;
        }
        jstring remote = environment->NewStringUTF(destination);
        jstring local = environment->NewStringUTF(source);
        jstring interface = environment->NewStringUTF(interface_name);
        if (method != nullptr && remote != nullptr && local != nullptr && interface != nullptr) {
            result = environment->CallBooleanMethod(host, method, descriptor, remote, local, interface) == JNI_TRUE ? 0 : -1;
        }
        if (environment->ExceptionCheck()) { environment->ExceptionClear(); result = -1; }
        environment->DeleteLocalRef(interface);
        environment->DeleteLocalRef(local);
        environment->DeleteLocalRef(remote);
        environment->DeleteLocalRef(host_class);
        environment->DeleteLocalRef(host);
    }
    if (environment->ExceptionCheck()) environment->ExceptionClear();
    if (attached) virtual_machine->DetachCurrentThread();
    return result;
}

void clear_host(JNIEnv *environment) {
    std::lock_guard<std::mutex> guard(callback_lock);
    if (socket_host != nullptr) environment->DeleteGlobalRef(socket_host);
    socket_host = nullptr;
}
}

extern "C" JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM *machine, void *) {
    virtual_machine = machine;
    return JNI_VERSION_1_6;
}

extern "C" JNIEXPORT jlong JNICALL
Java_org_bespin_avian_NativeEngine_start(JNIEnv *environment, jclass, jstring public_configuration,
                                        jbyteArray transport_credential, jobject callback, jstring policy_file) {
    std::lock_guard<std::mutex> lifecycle(lifecycle_lock);
    if (public_configuration == nullptr || transport_credential == nullptr || callback == nullptr || policy_file == nullptr) return JNI_FALSE;
    jsize length = environment->GetArrayLength(transport_credential);
    if (length <= 0 || length > 65) return JNI_FALSE;
    std::vector<unsigned char> credential(static_cast<size_t>(length));
    WipeMemory credential_guard{credential.data(), credential.size()};
    environment->GetByteArrayRegion(transport_credential, 0, length, reinterpret_cast<jbyte *>(credential.data()));
    if (environment->ExceptionCheck()) { environment->ExceptionClear(); return JNI_FALSE; }
    const char *configuration = environment->GetStringUTFChars(public_configuration, nullptr);
    if (configuration == nullptr) return JNI_FALSE;
    if (active_session != 0) {
        avian_android_stop_verified();
        if (active_tun_descriptor >= 0) { close(active_tun_descriptor); active_tun_descriptor = -1; }
        avian_android_set_socket_callback(nullptr);
        clear_host(environment);
        active_session = 0;
    }
    {
        std::lock_guard<std::mutex> guard(callback_lock);
        socket_host = environment->NewGlobalRef(callback);
    }
    if (socket_host == nullptr) {
        environment->ReleaseStringUTFChars(public_configuration, configuration);
        if (environment->ExceptionCheck()) environment->ExceptionClear();
        return JNI_FALSE;
    }
    const char *policy_path = environment->GetStringUTFChars(policy_file, nullptr);
    if (policy_path == nullptr) {
        environment->ReleaseStringUTFChars(public_configuration, configuration);
        clear_host(environment);
        return JNI_FALSE;
    }
    bool configured = setenv("AVIAN_TRAFFIC_POLICY_FILE", policy_path, 1) == 0;
    environment->ReleaseStringUTFChars(policy_file, policy_path);
    avian_android_set_socket_callback(prepare_socket);
    for (const char *name : {"ET_REQUIRE_VERIFIED_MEMBERSHIP", "AVIAN_OWNED_CONNECTIVITY_ONLY",
                             "ET_PRIORITY_FORWARDING", "ET_MEASURED_ROUTING", "ET_FAST_FAILOVER"}) {
        if (setenv(name, "1", 1) != 0) configured = false;
    }
    std::string encoded_credential(credential.begin(), credential.end());
    WipeMemory encoded_guard{encoded_credential.data(), encoded_credential.size()};
    if (setenv("ET_CREDENTIAL", encoded_credential.c_str(), 1) != 0) configured = false;
    int result = configured ? avian_android_start_verified(configuration, credential.data(), credential.size()) : -1;
    std::fill(credential.begin(), credential.end(), 0);
    std::fill(encoded_credential.begin(), encoded_credential.end(), '\0');
    environment->ReleaseStringUTFChars(public_configuration, configuration);
    if (result != 0) {
        unsetenv("ET_CREDENTIAL");
        unsetenv("AVIAN_TRAFFIC_POLICY_FILE");
        avian_android_set_socket_callback(nullptr);
        clear_host(environment);
    }
    if (result != 0) return 0;
    active_session = ++next_session;
    return static_cast<jlong>(active_session);
}

extern "C" JNIEXPORT jboolean JNICALL
Java_org_bespin_avian_NativeEngine_attachTun(JNIEnv *, jclass, jlong session, jint descriptor) {
    std::lock_guard<std::mutex> lifecycle(lifecycle_lock);
    if (descriptor < 0) return JNI_FALSE;
    if (session <= 0 || static_cast<uint64_t>(session) != active_session) { close(descriptor); return JNI_FALSE; }
    if (active_tun_descriptor >= 0 || avian_android_wait_instance_ready() != 0) {
        close(descriptor);
        return JNI_FALSE;
    }
    int result = set_tun_fd("avian-android", descriptor);
    if (result != 0) close(descriptor);
    else active_tun_descriptor = descriptor;
    return result == 0 ? JNI_TRUE : JNI_FALSE;
}

extern "C" JNIEXPORT jboolean JNICALL
Java_org_bespin_avian_NativeEngine_refreshTrust(JNIEnv *, jclass, jlong session) {
    std::lock_guard<std::mutex> lifecycle(lifecycle_lock);
    if (session <= 0 || static_cast<uint64_t>(session) != active_session) return JNI_FALSE;
    return avian_android_refresh_trust() == 0 ? JNI_TRUE : JNI_FALSE;
}

extern "C" JNIEXPORT void JNICALL
Java_org_bespin_avian_NativeEngine_stop(JNIEnv *environment, jclass, jlong session) {
    std::lock_guard<std::mutex> lifecycle(lifecycle_lock);
    if (session <= 0 || static_cast<uint64_t>(session) != active_session) return;
    active_session = 0;
    // Stop and join engine workers before releasing the callback's global reference.
    avian_android_stop_verified();
    // The Android engine launcher joins its runtime workers before this returns.
    // Its mobile TUN API borrows this descriptor; this boundary owns the duplicate.
    if (active_tun_descriptor >= 0) { close(active_tun_descriptor); active_tun_descriptor = -1; }
    avian_android_set_socket_callback(nullptr);
    unsetenv("ET_CREDENTIAL");
    unsetenv("AVIAN_TRAFFIC_POLICY_FILE");
    clear_host(environment);
}

extern "C" JNIEXPORT jboolean JNICALL
Java_org_bespin_avian_NativeEngine_setUnderlayPolicy(JNIEnv *environment, jclass, jstring policy) {
    std::lock_guard<std::mutex> lifecycle(lifecycle_lock);
    if (policy == nullptr) return JNI_FALSE;
    const char *document = environment->GetStringUTFChars(policy, nullptr);
    if (document == nullptr) return JNI_FALSE;
    int result = avian_android_set_policy(document);
    environment->ReleaseStringUTFChars(policy, document);
    return result == 0 ? JNI_TRUE : JNI_FALSE;
}

namespace {
jstring owned_string(JNIEnv *environment, const char *value) {
    if (value == nullptr) return nullptr;
    jstring result = environment->NewStringUTF(value);
    free_string(value);
    return result;
}
}

extern "C" JNIEXPORT jstring JNICALL
Java_org_bespin_avian_NativeEngine_publishServices(JNIEnv *environment, jclass, jstring document, jbyteArray owner_identity) {
    std::lock_guard<std::mutex> lifecycle(lifecycle_lock);
    if (document == nullptr || owner_identity == nullptr || environment->GetStringUTFLength(document) > 65536) return nullptr;
    jsize length = environment->GetArrayLength(owner_identity);
    if (length <= 0 || length > 65) return nullptr;
    std::vector<unsigned char> identity(static_cast<size_t>(length));
    WipeMemory identity_guard{identity.data(), identity.size()};
    environment->GetByteArrayRegion(owner_identity, 0, length, reinterpret_cast<jbyte *>(identity.data()));
    if (environment->ExceptionCheck()) return nullptr;
    const char *publication = environment->GetStringUTFChars(document, nullptr);
    if (publication == nullptr) return nullptr;
    const char *result = avian_android_publish_services(publication, identity.data(), identity.size());
    environment->ReleaseStringUTFChars(document, publication);
    return owned_string(environment, result);
}

extern "C" JNIEXPORT jstring JNICALL
Java_org_bespin_avian_NativeEngine_servicePage(JNIEnv *environment, jclass, jstring after, jint limit) {
    std::lock_guard<std::mutex> lifecycle(lifecycle_lock);
    if (limit <= 0 || limit > 100) return nullptr;
    const char *cursor = after == nullptr ? nullptr : environment->GetStringUTFChars(after, nullptr);
    if (after != nullptr && cursor == nullptr) return nullptr;
    const char *result = avian_android_service_page(cursor, static_cast<size_t>(limit));
    if (cursor != nullptr) environment->ReleaseStringUTFChars(after, cursor);
    return owned_string(environment, result);
}

extern "C" JNIEXPORT jstring JNICALL
Java_org_bespin_avian_NativeEngine_eventPage(JNIEnv *environment, jclass, jlong after, jint limit) {
    std::lock_guard<std::mutex> lifecycle(lifecycle_lock);
    if (after < 0 || limit <= 0 || limit > 100) return nullptr;
    return owned_string(environment, avian_android_event_page(static_cast<uint64_t>(after), static_cast<size_t>(limit)));
}

extern "C" JNIEXPORT jint JNICALL
Java_org_bespin_avian_NativeEngine_updateUnderlays(JNIEnv *environment, jclass, jlong session, jstring update) {
    std::lock_guard<std::mutex> lifecycle(lifecycle_lock);
    if (session <= 0 || static_cast<uint64_t>(session) != active_session || update == nullptr) return -1;
    const char *document = environment->GetStringUTFChars(update, nullptr);
    if (document == nullptr) return -1;
    const char *path = avian_android_update_underlays(document);
    bool success = path != nullptr && setenv("AVIAN_TRAFFIC_POLICY_FILE", path, 1) == 0;
    if (path != nullptr) free_string(path);
    bool policy_current = false;
    if (success) {
        jobject host;
        {
            std::lock_guard<std::mutex> guard(callback_lock);
            host = socket_host == nullptr ? nullptr : environment->NewLocalRef(socket_host);
        }
        if (host == nullptr) success = false;
        else {
            jclass host_class = environment->GetObjectClass(host);
            jmethodID activate = environment->GetMethodID(host_class, "policyActivated", "()Z");
            if (activate == nullptr) success = false;
            else policy_current = environment->CallBooleanMethod(host, activate) == JNI_TRUE;
            if (environment->ExceptionCheck()) { environment->ExceptionClear(); success = false; }
            environment->DeleteLocalRef(host_class);
            environment->DeleteLocalRef(host);
        }
    }
    int result = success ? (policy_current ? avian_android_reconcile_underlays(document) : 1) : -1;
    environment->ReleaseStringUTFChars(update, document);
    return result;
}

extern "C" JNIEXPORT jstring JNICALL
Java_org_bespin_avian_NativeEngine_status(JNIEnv *environment, jclass) {
    std::lock_guard<std::mutex> lifecycle(lifecycle_lock);
    return owned_string(environment, avian_android_status());
}

extern "C" JNIEXPORT jstring JNICALL
Java_org_bespin_avian_AvianProvisioning_provision(JNIEnv *environment, jclass, jint operation, jstring directory, jstring document, jstring fingerprint, jbyteArray identity_bytes, jbyteArray transport_bytes) {
    std::lock_guard<std::mutex> lifecycle(lifecycle_lock);
    if (active_session != 0 || operation < 0 || operation > 2 || directory == nullptr || document == nullptr || fingerprint == nullptr || identity_bytes == nullptr || transport_bytes == nullptr) return nullptr;
    if (environment->GetStringUTFLength(document) > 16 * 1024 * 1024 || environment->GetStringUTFLength(directory) > 4096 || environment->GetStringUTFLength(fingerprint) > 128) return nullptr;
    jsize identity_length = environment->GetArrayLength(identity_bytes);
    jsize transport_length = environment->GetArrayLength(transport_bytes);
    if (identity_length <= 0 || identity_length > 65 || transport_length <= 0 || transport_length > 65) return nullptr;
    std::vector<unsigned char> identity(static_cast<size_t>(identity_length));
    std::vector<unsigned char> transport(static_cast<size_t>(transport_length));
    WipeMemory identity_guard{identity.data(), identity.size()};
    WipeMemory transport_guard{transport.data(), transport.size()};
    environment->GetByteArrayRegion(identity_bytes, 0, identity_length, reinterpret_cast<jbyte *>(identity.data()));
    environment->GetByteArrayRegion(transport_bytes, 0, transport_length, reinterpret_cast<jbyte *>(transport.data()));
    if (environment->ExceptionCheck()) return nullptr;
    const char *directory_text = environment->GetStringUTFChars(directory, nullptr);
    const char *document_text = environment->GetStringUTFChars(document, nullptr);
    const char *fingerprint_text = environment->GetStringUTFChars(fingerprint, nullptr);
    const char *result = nullptr;
    if (directory_text != nullptr && document_text != nullptr && fingerprint_text != nullptr) {
        result = avian_android_provision(static_cast<uint8_t>(operation), directory_text, document_text, fingerprint_text, identity.data(), identity.size(), transport.data(), transport.size());
    }
    if (directory_text != nullptr) environment->ReleaseStringUTFChars(directory, directory_text);
    if (document_text != nullptr) environment->ReleaseStringUTFChars(document, document_text);
    if (fingerprint_text != nullptr) environment->ReleaseStringUTFChars(fingerprint, fingerprint_text);
    return owned_string(environment, result);
}
