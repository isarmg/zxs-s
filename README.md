# ZXS

面向高速局域网与多物理盘的文件与线性版本管理系统。服务端在磁盘上直接保存真实目录与只读原件，桌面端映射本机目录供任意软件编辑，移动端支持流播、离线查阅与相册上传。

[下载](#下载) · [快速开始](#快速开始) · [文件结构](#文件结构) · [ZIP 工作区](#zip-工作区) · [使用与协议手册](docs/manual.md) · [设计规格书](zxs-design-document.md) · [MIT](LICENSE)

## 特点

- **裸盘直读**：不设私有对象池，不以哈希或 UUID 命名目录；每个历史版本均为 `0444` 只读原件，拔下硬盘即可在其他电脑直接读取。
- **显式版本**：桌面端日常保存只写本地，显式提交才生成新版本；传输、迁移与覆盖前全程校验 BLAKE3。
- **目录整包**：含 `.zxsignore` 的目录在本地直接作为普通文件夹编辑，提交时打包为标准 `.zip` 单容器收发，移动端免全量解压即可逐层浏览。
- **轻量原生**：服务端与桌面端各为单一可执行文件（内置本机网页界面），iOS 与 Android 采用原生界面调用同一套同步 Rust 内核。

## 下载

| 平台 | 说明 | 直达链接 |
| --- | --- | --- |
| Windows 11 x64 | 桌面客户端安装包（含 `zxs-cli` 与网页界面） | [ZXS-S-Windows-x64.msi](https://github.com/isarmg/zxs-s/releases/latest/download/ZXS-S-Windows-x64.msi) |
| macOS 15+ (ARM64) | 桌面客户端（`zxs-cli` 与网页界面） | [zxs-cli-0.1.14-aarch64-apple-darwin.tar.gz](https://github.com/isarmg/zxs-s/releases/download/v0.1.14/zxs-cli-0.1.14-aarch64-apple-darwin.tar.gz) |
| Linux x86_64（桌面） | 桌面客户端（Linux 6.6+） | [zxs-cli-0.1.14-x86_64-unknown-linux-gnu.tar.gz](https://github.com/isarmg/zxs-s/releases/download/v0.1.14/zxs-cli-0.1.14-x86_64-unknown-linux-gnu.tar.gz) |
| Linux x86_64（服务端） | 存储服务 `zxs` + 磁盘准备工具 `zxs-prepare` | [zxs-server-0.1.14-x86_64-unknown-linux-gnu.tar.gz](https://github.com/isarmg/zxs-s/releases/download/v0.1.14/zxs-server-0.1.14-x86_64-unknown-linux-gnu.tar.gz) |
| Android 13+ (ARM64) | 原生移动客户端（正式签名 APK） | [ZXS-S-Android-arm64.apk](https://github.com/isarmg/zxs-s/releases/latest/download/ZXS-S-Android-arm64.apk) |
| iOS 17+ (ARM64) | 原生移动客户端（未签名 IPA，签名后装机） | [ZXS-S-iOS-arm64-unsigned.ipa](https://github.com/isarmg/zxs-s/releases/latest/download/ZXS-S-iOS-arm64-unsigned.ipa) |

校验清单见 [SHA256SUMS](https://github.com/isarmg/zxs-s/releases/latest/download/SHA256SUMS)，历史版本与独立磁盘准备包见 [Releases](https://github.com/isarmg/zxs-s/releases)。

## 快速开始

1. **启动服务端**：准备数据盘与回收盘目录，登记口令并启动服务（默认监听 `0.0.0.0:7738`；存量盘转换见 [磁盘准备说明](tools/zxs-prepare/README.md)）：

```sh
zxs-prepare init /srv/zxs/data/disk1 --kind data
zxs-prepare init /srv/zxs/recycle/rec1 --kind recycle
printf '%s\n' '<口令>' | zxs init --data-dir /srv/zxs/data --recycle-dir /srv/zxs/recycle
zxs serve --data-dir /srv/zxs/data
```

2. **连接客户端**：桌面端执行 `zxs-cli ui` 打开本机管理网页（命令行参数见 [使用手册](docs/manual.md)）；移动端输入服务器地址与口令即可浏览、流播与上传。

## 文件结构

系统元数据统一收敛在唯一保留名 `.zxs` 下，不占用业务文件名。

### 服务端磁盘

数据根与回收根下的一级子目录分别对应各块独立磁盘：

```text
<数据根>/
├── .zxs/                            # 全局配置与可重建的 SQLite 索引
└── disk1/                           # 物理数据盘（客户端显示为 /disk1）
    ├── .zxs/                        # 盘身份标记与本盘暂存区
    └── 文档/方案.docx/              # 容器目录名 = 真实文件名
        ├── .zxs                     # 容器元数据（ID、标签、保留版本范围）
        ├── v000001/
        │   ├── .zxs                 # 版本元数据（时间、大小、BLAKE3）
        │   └── 方案.docx            # 0444 只读原件
        └── v000002/
            ├── .zxs
            └── 方案.docx

<回收根>/
└── rec1/                            # 物理回收盘（多盘按剩余空间自动分配）
    ├── .zxs/
    └── 2026-11-28_<回收ID>/         # 平铺保存移入回收站的原容器与全部版本
```

### 客户端工作区

桌面工作区按 `<数据盘>/<目录>` 镜像本地可编辑文件；移动端用系统安全存储保存凭据，并在私有缓存中按需保留最近打开的原件与目录包成员。

```text
<桌面工作区>/
├── .zxs/                            # 本地追踪库 workspace.sqlite 与覆盖前自动备份
└── disk1/
    ├── 文档/方案.docx               # 普通文件：本地直接修改，显式提交后升版
    └── 工程/源码.zip/               # ZIP 工作区：本地以普通文件夹形态读写
        ├── .zxsignore               # 目录包标记与忽略规则
        ├── src/                     # 纳入版本打包的目录与文件
        └── target/                  # 被规则忽略、仅保留在本机的目录
```

## ZIP 工作区

将代码库、笔记库或工程目录作为单个容器管理，避免为大量碎文件逐一建容器：

- **桌面端**：在工作区子目录创建 `.zxsignore`（或执行 `zxs-cli init-bundle`），目录以 `.zip` 后缀文件夹形态直接在本地编辑；提交时自动过滤忽略项并整包上传，检出与同步时保留本地忽略文件不变。
- **服务端**：仅作为单个标准 `.zip` 原件存储与校验，不解析包内结构，裸盘可直接解压。
- **移动端**：打开时校验整包并在本机建立目录索引，支持逐层浏览、按需提取单个文件及断网离线查阅。

## 文档索引

| 文档 | 内容 |
| --- | --- |
| [docs/manual.md](docs/manual.md) | CLI 完整命令、本机配置路径、服务端离线运维、源码构建、发行签名与 HTTP 接口表 |
| [tools/zxs-prepare/README.md](tools/zxs-prepare/README.md) | 空盘初始化、存量数据盘转换与挂载说明 |
| [zxs-design-document.md](zxs-design-document.md) | 架构边界、多盘事务恢复、冲突处理与设计指标 |
| [tests/README.md](tests/README.md) | 单元、集成、桌面、浏览器与移动端测试说明 |
