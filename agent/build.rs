fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if needs_getrandom_shim(&target) {
        cc::Build::new()
            .file("getrandom_shim.c")
            .compile("getrandom_shim");
    }
}

/// Tier-3 glibc Linux 目标可能使用旧版 glibc（< 2.25），缺少 getrandom() C 函数。
///
/// Rust 标准库和 getrandom crate v0.3+ 在 glibc 环境下调用 libc::getrandom()，
/// 如果 glibc 版本太旧，链接器会报 `undefined reference to 'getrandom'`。
///
/// 通过弱符号提供 getrandom() 实现：如果 glibc 已有此函数则被覆盖，无副作用。
fn needs_getrandom_shim(target: &str) -> bool {
    // MIPS 系列 (tier 3)：无预编译 std，cross Docker 镜像 glibc 较旧
    let is_mips_gnu =
        target.starts_with("mips") && target.contains("linux") && target.contains("gnu");
    // ARMv5TE (tier 3)：同上
    let is_armv5te_gnu =
        target.contains("armv5te") && target.contains("linux") && target.contains("gnu");
    // PowerPC 32-bit (tier 3)：同上
    let is_powerpc_gnu = target == "powerpc-unknown-linux-gnu";

    is_mips_gnu || is_armv5te_gnu || is_powerpc_gnu
}
