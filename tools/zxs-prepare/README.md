# ZXS 磁盘准备工具

`zxs-prepare` 是独立的桌面命令行程序，包含 Linux、macOS 和 Windows 共用的本机浏览器界面。工具直接写入 ZXS 磁盘格式。构建使用 Rust 1.100.0 和仓库已有的第三方离线源码。

## 构建和启动

```sh
cd tools/zxs-prepare
cargo build --offline --locked --release
./target/release/zxs-prepare ui
```

Windows 在同一目录执行 `.\target\release\zxs-prepare.exe ui`。浏览器打开后输入所选磁盘的挂载目录，例如 `/mnt/disk1`、`/Volumes/disk1` 或 `D:\ZXS`。终端会显示可直接打开的本机地址。

## 准备空盘

选择已有的空目录。网页可以选择数据盘或回收盘用途。CLI 使用 `init`。

```sh
zxs-prepare init /mnt/disk1 --kind data
zxs-prepare init /mnt/rec1 --kind recycle
```

工具在盘根的 `.zxs/disk.toml` 写入盘用途和盘身份。

## 准备已有数据的盘

选择尚未登记的盘目录，准备期间停止其他程序向该目录写入。网页可逐层浏览，在项目一行选择处理方式。CLI 用 `scan` 查看目录，用可重复的 `--bundle` 指定打包项。

```sh
zxs-prepare scan /mnt/disk1
zxs-prepare scan /mnt/disk1 --within '文档'
zxs-prepare prepare /mnt/disk1 --bundle '照片/旅行' --bundle '文档/手册.pdf'
```

- 完整逻辑路径包含服务端挂载名和转换后的最终名称，上限为 1,024 个 UTF-8 字节。默认使用所选目录名作为挂载名。两者不同时，请在网页填写「服务端挂载名」，或在 `prepare` 加 `--mount-name <名称>`。
- 默认保留目录层级、空目录和文件名。每个普通文件生成独立容器，原件放入 `v000001/`。已有 ZIP 可直接作为单文件容器保留。
- 选择 **ZIP 目录包** 会把该文件或目录打成一个容器。目录名已有 `.zip` 后缀时沿用原名。包内首项是 `.zxsignore`，存在该文件时按其中的规则打包，否则全部打包。
- 工具先生成并同步盘内暂存，随后把原件移到 `.zxs/originals/` 并发布容器。准备需要足够空间同时存放原数据和转换结果。出现名称冲突或不支持的文件类型时原数据留在原处。
- 切换已开始时可以在网页选择「继续准备」，或运行 `zxs-prepare resume /mnt/disk1`。原件和准备结果都在所选磁盘上。

## 接入服务端

把准备好的目录作为数据根或回收根的一级挂载目录。首次配置服务端运行 `zxs init --data-dir <数据根> --recycle-dir <回收根>`。向现有服务新增磁盘时，在服务停止期间运行 `zxs --data-dir <数据根> sync-disks`。两种方式均沿用工具写入的盘身份。切换尚未写出 `.zxs/disk.toml` 时，服务端会跳过此盘。

Linux 服务端使用支持 POSIX 权限的文件系统。Linux 和 macOS 上工具将原件设为 `0444`，Windows 上则设置只读属性。若迁入 Linux 文件系统时未保留 `0444`，在登记前对该准备目录补齐原件模式。以下命令跳过盘根的系统目录和全部 `.zxs` 元数据文件。

```sh
find /mnt/disk1 -type d -name .zxs -prune -o -type f ! -name .zxs -exec chmod 0444 -- {} +
```

登记后可用 `zxs --data-dir <数据根> verify` 核对原件长度与 BLAKE3。

## 验证

```sh
cargo test --offline --locked
cargo clippy --offline --locked --all-targets --all-features -- -D clippy::pedantic -D warnings
```
