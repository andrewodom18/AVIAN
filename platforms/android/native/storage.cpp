#include <jni.h>
#include <cerrno>
#include <cstring>
#include <fcntl.h>
#include <linux/fs.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

namespace {
struct BorrowedString {
    JNIEnv *environment;
    jstring value;
    const char *bytes;
    BorrowedString(JNIEnv *environment, jstring value)
        : environment(environment), value(value),
          bytes(value == nullptr ? nullptr : environment->GetStringUTFChars(value, nullptr)) {}
    ~BorrowedString() {
        if (bytes != nullptr) environment->ReleaseStringUTFChars(value, bytes);
    }
};

struct Descriptor {
    int value;
    ~Descriptor() { if (value >= 0) close(value); }
};

bool private_owned(const struct stat &metadata, bool directory) {
    return metadata.st_uid == getuid() && (metadata.st_mode & 0077) == 0 &&
           (directory ? S_ISDIR(metadata.st_mode)
                      : S_ISREG(metadata.st_mode) && metadata.st_nlink == 1);
}

bool leaf_name(const char *name) {
    return name != nullptr && name[0] != '\0' && std::strlen(name) <= 255 &&
           std::strchr(name, '/') == nullptr && std::strcmp(name, ".") != 0 &&
           std::strcmp(name, "..") != 0;
}
}

// Android app SELinux policies reject hard links. Commit the already flushed
// ciphertext with a single no-replace rename, anchored to the private directory.
extern "C" JNIEXPORT jint JNICALL
Java_org_bespin_avian_NativeStorage_commit(JNIEnv *environment, jclass,
                                         jstring directory, jstring temporary, jstring destination) {
    BorrowedString parent(environment, directory);
    if (parent.bytes == nullptr) return ENOMEM;
    BorrowedString source(environment, temporary);
    if (source.bytes == nullptr) return ENOMEM;
    BorrowedString target(environment, destination);
    if (target.bytes == nullptr) return ENOMEM;
    if (parent.bytes[0] != '/' || !leaf_name(source.bytes) || !leaf_name(target.bytes)) return EINVAL;
    Descriptor parent_descriptor{open(parent.bytes, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW)};
    if (parent_descriptor.value < 0) return errno;
    struct stat parent_metadata{};
    if (fstat(parent_descriptor.value, &parent_metadata) != 0) return errno;
    if (!private_owned(parent_metadata, true)) return EACCES;
    Descriptor source_descriptor{openat(parent_descriptor.value, source.bytes, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)};
    if (source_descriptor.value < 0) return errno;
    struct stat source_metadata{};
    if (fstat(source_descriptor.value, &source_metadata) != 0) return errno;
    if (!private_owned(source_metadata, false)) return EACCES;
    if (fsync(parent_descriptor.value) != 0) return errno;
    if (syscall(SYS_renameat2, parent_descriptor.value, source.bytes,
                parent_descriptor.value, target.bytes, RENAME_NOREPLACE) != 0) return errno;
    if (fsync(parent_descriptor.value) != 0) return errno;
    return 0;
}
