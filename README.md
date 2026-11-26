# ZXS

ZXS 是面向局域网的文件和版本管理系统。Linux 服务端把每个版本保留为可以直接打开的原件。同步客户端提供命令行、本机浏览器界面，以及供 iOS 和 Android 原生应用调用的 C 接口。

| 目录 | 内容 |
| --- | --- |
| [zxs-server](zxs-server/) | Linux 存储、HTTP 服务和离线运维 |
| [zxs-client](zxs-client/) | 同步 Rust 内核、桌面 CLI、浏览器界面与移动端 |
| [tests/smoke.py](tests/smoke.py) | 同时启动服务端和客户端的端到端测试 |
| [tests/browser.mjs](tests/browser.mjs) | 本机网页、目录包和会话自动续接的浏览器测试 |

发布包覆盖 Linux x86_64 和 ARM64 的服务端与客户端，以及 Windows x64 和 Apple Silicon macOS 的桌面客户端。Windows MSI 支持选择安装目录，向导中会显示项目许可证和安装结果。下载后可用 `SHA256SUMS` 核对发布包，原生二进制压缩包内的 `BUILD.txt` 记录版本、提交和目标平台。[MIT 许可证](LICENSE) · [下载发布包](https://github.com/isarmg/zxs-s/releases)

从 v0.1.1 起，版本标签与 Cargo 版本一致。标签通过平台验收后，GitHub Release 一次发布各平台压缩包、Windows MSI 和校验清单。部署时先升级服务端，再升级客户端，以使用目录和容器的批量查询。

## 从空目录开始

仓库通过 `rust-toolchain.toml` 固定 Rust 1.100.0。两个 Rust 项目各自锁定依赖和 vendor 源，构建需要 C 编译器和链接器。x86-64 默认以 x86-64-v3 为基线。服务端需要 Linux 6.6+。客户端目标为 Linux 6.6+、Apple Silicon macOS 15+ 和 Windows 11 24H2+ 的 MSVC 环境。

在仓库根目录分别构建。

```sh
(cd zxs-server && cargo build --release --offline --locked)
(cd zxs-client && cargo build --release --offline --locked -p zxs-cli)
```

先创建数据盘和回收盘的一级目录，再初始化存储。单盘环境可用普通目录 `disk1` 和 `rec1`，多盘环境在各一级目录挂载对应的磁盘。服务端默认监听 `0.0.0.0:7738`。可通过 `serve --listen <地址:端口>` 或数据根的 `.zxs/config.toml` 更改监听地址。

```sh
mkdir -p ./zxs-data/disk1 ./zxs-recycle/rec1
printf '%s\n' '替换成你自己的口令' | ./zxs-server/target/release/zxs init \
  --data-dir ./zxs-data --recycle-dir ./zxs-recycle
./zxs-server/target/release/zxs serve --data-dir ./zxs-data
```

另开终端，登录、提交并查看文件。Windows 默认使用系统桌面下的 `ZXS-S-Workspace`，macOS 和 Linux 默认使用启动时的目录。在网页的「工作区 → 选择工作区」中可选择一个已有目录，应用会记住所选路径。要搬动原工作区时，先关闭客户端与编辑器，用系统移动整个目录（含 `.zxs`），再选择新位置。显式 `--workspace` 优先，尚未保存位置时可用 `ZXS_WORKSPACE` 指定默认值。

```sh
mkdir -p demo-work/disk1/文档
printf '%s\n' 'first version' > demo-work/disk1/文档/notes.txt
printf '%s\n' '替换成你自己的口令' | ./zxs-client/target/release/zxs-cli login
./zxs-client/target/release/zxs-cli --workspace demo-work commit disk1/文档/notes.txt --tag 笔记
./zxs-client/target/release/zxs-cli ls /disk1/文档
./zxs-client/target/release/zxs-cli --workspace demo-work ui
```

`ui` 在 `127.0.0.1` 的随机端口启动，并用一次性地址打开默认浏览器。「文件」页按真实目录浏览，也可按名称或标签搜索全库。选中文件后，可用工具栏管理标签。详情栏可以预览图片或音视频原件，并显示文件大小和历史版本。历史版本可恢复到工作区，旧版内容按待提交的本地修改处理。

本地工作区镜像服务端文件，网页「工作区」列出本地变更。在「文件」页可修改文件和文件夹的名称或目录路径，受影响的干净追踪文件随远端路径移动，遇到本地修改会提示处理。空目录在显式同步所选目录时建立。浏览、登录、刷新与新建远端目录按需读取元数据。点击「提交」沿用工作区内的目录和文件名。普通目录可用「提交当前目录」递归提交新增和修改的文件，保留目录层级。通过工作区工具栏创建 `.zxsignore` 后会进入该目录，可在网页里逐层浏览并提交整包。新检出的目录包以完整 `.zip` 名称落盘为文件夹，已有追踪项沿用已记录的路径。

### 工作区与版本

- `checkout <id> [--version <N>] [--to <本地路径>]` 检出文件到工作区。恢复旧版后，显式 `commit` 才发布新版。已有本地改动时可用 `--backup` 确认备份后恢复。
- `status` 使用大小和纳秒级修改时间检查工作区，也列出新增项、缺失项和被忽略的非法 `.zxsignore`。网页浏览工作区目录时按当前子树扫描。
- `sync` 更新已追踪文件，`sync --folder /disk1/文档` 显式检出并同步所选目录及其子目录。网页可用「同步已追踪」或「同步此目录」。同步保留本地修改，覆盖或移动前核对本地 BLAKE3。
- `refresh` 执行同步后列出本地工作区状态。同步失败或存在逐项错误时，`sync` 和 `refresh` 都返回非零退出码。逐项错误仍保留在各自的输出中。
- 网页的「提交当前目录」会复核看起来未改动的已追踪文件及目录包成员，保留原大小和修改时间的编辑也会提交。
- `commit <本地路径>` 给已跟踪的文件追加版本。散文件先计算 BLAKE3，上传字节与之不符时拒绝发布。首次提交沿用工作区内的相对父目录。位于工作区根目录的文件须通过 `--folder /disk1` 指定目标盘。可用 `--name` 和 `--tag` 设置远端名称和标签。
- 遇到版本冲突时，可用 `commit <路径> --on-conflict append` 追加到最新版本，`--on-conflict new --name <文件名>` 另存，或 `--on-conflict pull` 先备份再检出远端。备份位置随结果返回。
- `init-bundle <本地目录>` 在已登记数据盘下的子目录中创建 `.zxsignore`。目录及其后代已有文件容器时，客户端会忽略手工添加的 `.zxsignore` 并提示。提交目录包会生成首条目为 `.zxsignore` 的 ZIP/ZIP64，其余条目按 NFC 路径排序。检出更新会保留本地被忽略的文件。
- `mkdir <目录>` 创建空目录，`mv-folder <原目录> <新目录>` 移动整棵目录树，`rmdir <目录>` 删除空目录。加 `--recursive` 会将子树内的容器移入回收站。
- `detail <id>` 查看容器和版本历史。`patch <id>` 可以用 `--folder`、`--name`、`--tags` 或 `--clear-tags` 修改元数据。
- `logout` 清除本机保存的服务器登录信息。
- `recycle ls` 查看回收站，`recycle move <容器 ID>` 回收文件，`restore <回收 ID>` 还原。用 `recycle purge <回收 ID>` 或 `recycle purge --all` 彻底删除。

### 服务端运维

执行离线命令前先停止 `serve`。所有命令共用 `--data-dir` 和同一把进程锁。回收目录在初始化时绑定。`ZXS_DATA_DIR` 和 `ZXS_RECYCLE_DIR` 可以代替路径参数。

```sh
zxs --data-dir /srv/zxs import /mnt/source --folder /disk1/导入
zxs --data-dir /srv/zxs sync-disks
zxs --data-dir /srv/zxs prune --keep-last 5
zxs --data-dir /srv/zxs verify
zxs --data-dir /srv/zxs rebuild-index
printf '%s\n' '替换成你自己的新口令' | zxs --data-dir /srv/zxs reset-password
```

已发布的原件位于 `<数据根>/<数据盘>/<目录>/<容器名称>/vNNNNNN/<提交时的文件名>`，权限为 `0444`。空目录保留到显式删除。回收批次位于 `<回收根>/<回收盘>/<日期>_<回收 ID>/`，其中的 `.zxs/recycle.json` 记录原路径。跨盘操作先在目标盘暂存并核对各版本的 BLAKE3，再切换目录。跨盘移动和回收还核对索引中的版本清单。跨盘迁移或递归回收目录时，服务端等待已受理的相交写入完成，期间源树拒绝新写入并返回 `423 Locked`。读取和其他路径的写入照常。服务启动时核对 `layout = 2` 以及所有登记盘的 `.zxs/disk.toml`。每个容器在磁盘上记录首个保留版本和最新版。巡检、重建和还原在该范围内检查缺失版本，即使 SQLite 索引已丢失。`verify` 校验数据盘和已挂载的回收盘，遇到离线回收盘时提示盘名并继续。巡检和重建会报告缺少描述文件的回收批次。已有索引可用时也检查其中记录的版本目录。

服务端和离线运维只接受布局 2。配置中的布局不匹配时，命令报错退出。

## HTTP/1.1 契约

受保护请求在请求头中使用 `Authorization: Bearer <token>`。客户端安全保存服务端地址和密码，服务端重启后自动重新登录。执行 `reset-password` 后需要使用新密码登录。上传成功却丢失响应时，再次提交会核对远端最新内容的 BLAKE3，相同内容会直接认领。

| 方法和路径 | 请求与结果 |
| --- | --- |
| `POST /sessions` | JSON `{"password":"…"}`。返回 `201` 和 `token`，会话保存在服务进程内存中。 |
| `DELETE /sessions/current` | 撤销当前会话，返回 `204`。 |
| `POST /containers` | 上传首版。上传参数为 `folder`、`name`、可选的 `tags`。返回 `201` 和 `{container, version}`。 |
| `POST /containers/{id}/versions` | 上传参数为 `expected_version`。返回 `201` 和 `{container, version}`。 |
| `PATCH /containers/{id}` | JSON `folder`、`name`、`tags` 均可选。返回更新后的容器。 |
| `GET /containers` | 支持 `folder`、`tag`、`q`、`cursor`、`limit`。返回 `containers` 和 `next_cursor`，容器项含 `latest_blake3`。也可单独传入逗号分隔的 `ids` 批量读取 100 个以内的容器摘要。 |
| `GET /containers/{id}` | 返回 `container` 和升序的 `versions`。 |
| `GET` 或 `HEAD /containers/{id}/versions/{version}/payload` | 完整原件或单区间 `Range`，附强 BLAKE3 `ETag`。区间无效返回 `416`。 |
| `DELETE /containers/{id}` | 将容器移入回收站，返回 `{recycle, recycle_disk, container}`。可附 `expected_version` 校验版本。 |
| `GET /recycle` | 支持 `cursor`、`limit`，返回 `entries` 和 `next_cursor`。 |
| `POST /recycle/{id}/restore` | JSON `folder` 和 `name` 均可选。返回还原后的容器。 |
| `DELETE /recycle/{id}` 或 `DELETE /recycle` | 彻底删除一条或全部回收记录。 |
| `GET /folders?folder=<路径>` | 返回直接子目录，包含空目录。根目录返回已登记的数据盘。带 `recursive=true` 时返回该路径下的目录树。 |
| `POST /folders` 或 `PATCH /folders` | 传 `{folder}` 创建目录，或传 `{from,to}` 移动目录。 |
| `DELETE /folders?folder=<路径>` | 删除空目录。带 `recursive=true` 时将其中的容器全部回收。 |

上传正文是 `application/octet-stream`。`Zxs-Params` 是无填充 Base64URL 编码的 UTF-8 JSON。`Content-Length` 声明字节数，可选的 `Zxs-Blake3` 声明正文哈希。空文件以 `Content-Length: 0` 正常上传。服务端支持 `Expect: 100-continue` 预检。

## 移动端

- iOS 17+。在 Apple Silicon Mac 上安装 `aarch64-apple-ios` 和 `aarch64-apple-ios-sim` Rust 目标，打开 `zxs-client/apps/ios/ZXS.xcodeproj`。真机运行需在 Xcode 中设置签名团队。
- Android 13+。安装 Android SDK 35、Build Tools 35.0.0、NDK `27.2.12479018`、CMake `3.22.1`、JDK 17、Gradle 8.9 和 Rust 的 `aarch64-linux-android` 目标。用 Android Studio 打开 `zxs-client/apps/android`，或在该目录运行 `gradle :app:assembleDebug`。
- 手机列表先读目录和文件元数据。打开 ZIP 时下载完整原件并校验长度和 BLAKE3。重新打开时核对本机原件缓存的 BLAKE3。已打开的 ZIP 工作区复用本机目录索引，断网时仍可浏览、校验和解压所选成员。普通音视频通过系统播放器流播，文档通过系统预览打开。
- iOS 的 `ZXS` 和 `ZXSFileProvider` 目标使用同一签名团队。登录后，在系统「文件 → 浏览 → 位置 → ZXS」可只读浏览真实目录，打开或复制文件原件。ZIP 以压缩包显示，在 ZXS 应用内打开目录包可浏览成员。首次使用可通过「浏览 → … → 编辑」启用该位置。
- 两个应用都通过系统相册选择器备份原件到 `/<数据盘>/相册`。单盘时自动选择，多盘时记住所选目标盘。备份显示处理进度及每项结果，可单独重试失败项。

移动端原件和 ZIP 成员各有本机缓存，常规上限分别为 64 份和 512 MiB。iOS 用 Keychain 保存登录凭据，Android 用系统 Keystore 管理的 AES-256-GCM 密钥加密保存。服务端重启后应用使用已保存的口令自动续接。

## 验证和依赖更新

CI 会在两个 Rust 工程分别执行 `cargo clippy --all-targets --all-features -- -D clippy::pedantic -D warnings`。

在两个项目各自运行 `cargo test --offline --locked --workspace`。构建调试二进制后，从仓库根运行 `python3 tests/smoke.py`。Linux 上可加 `ZXS_SMOKE_RECYCLE_PARENT=/dev/shm` 验证跨文件系统回收和还原。macOS 和 Linux 可运行 `python3 tests/workspace_selection.py` 核对目录选择、原目录保留和重启后的设置。两个 `tools/sync_vendor.py --check` 分别核对依赖清单，需 Python 3.11+。更新依赖时在各项目更新 `Cargo.lock`，再运行自己的 `tools/sync_vendor.py`。

CI 还会在 Windows 上运行历史版本恢复用例，在 iOS 模拟器上安装并启动应用，以及在 Android 模拟器上验证登录、ZIP 离线浏览和服务端重启后的续接。Android 模拟器用 x86_64 调试包，常规 APK 仍为 ARM64。

Linux 上可另用 Node.js 22 运行真实 Chromium 测试。Playwright 只用于测试，桌面客户端的网页仍由 Rust 二进制直接提供。

```sh
cd tests
npm ci --ignore-scripts
npx --no-install playwright install --with-deps --only-shell chromium
npm test
```

吞吐、冷启动和 4K/8K 播放目标需要在目标网卡、NVMe 和终端上实测。
