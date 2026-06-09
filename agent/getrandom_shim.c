/*
 * 为旧版 glibc（< 2.25）提供 getrandom() 函数
 *
 * glibc 2.25 才引入了 getrandom() C 函数，但 getrandom 系统调用在 Linux 3.17 就已可用。
 * Rust 标准库（std::sys::random::linux）在 glibc 目标上调用 libc::getrandom()，
 * getrandom crate v0.3+ 也改用了 libc::getrandom()（而非 v0.2 的 libc::syscall()）。
 * 当交叉编译环境的 glibc 版本 < 2.25 时，链接器会报 undefined reference to 'getrandom'。
 *
 * 使用弱符号：如果 glibc 已提供 getrandom()，则优先使用 glibc 版本。
 */
#include <sys/syscall.h>
#include <unistd.h>
#include <stddef.h>

__attribute__((weak))
ssize_t getrandom(void *buf, size_t buflen, unsigned int flags) {
    return syscall(SYS_getrandom, buf, buflen, flags);
}
