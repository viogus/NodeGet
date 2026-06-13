# 安装 NodeGet

对于想要体验 NodeGet 的用户，可以非常快速地安装并获得体验

- 对于绝大部分用户，推荐使用 [自动化脚本安装](./install-script.md)
- 对于 Windows 用户和需要较高自定义安装需求的用户，可以参考 [手动安装](./manual-install.md)
- 除此之外，还可以通过 [Docker 安装](./docker.md)

## 支持的平台

GitHub Releases 会发布预编译的 Server 和 Agent 二进制文件。下面列出当前 CI 覆盖的主要平台：

| 平台      | 架构      | libc       | Server | Agent |
|---------|---------|------------|--------|-------|
| Linux   | x86_64  | musl / gnu | ✓      | ✓     |
| Linux   | aarch64 | musl / gnu | ✓      | ✓     |
| Linux   | armv7   | gnueabihf  | ✓      | ✓     |
| Windows | x86_64  | msvc       | ✓      | ✓     |
| Windows | aarch64 | msvc       | ✓      | ✓     |
| macOS   | aarch64 | -          | ✓      | ✓     |

Agent 额外覆盖了大量 Linux 目标平台，包括
i686、arm、armv7、mips/mipsel/mips64/mips64el、powerpc、powerpc64、powerpc64le、s390x、riscv64gc 等，详见仓库的 [
`.github/workflows/release.yml`](https://github.com/NodeSeekDev/NodeGet/blob/main/.github/workflows/release.yml)。