# ZXS

ZXS 是面向局域网的文件和版本管理系统。Linux 服务端把每个版本保留为可以直接打开的原件。同步客户端提供命令行、本机浏览器界面，以及供 iOS 和 Android 原生应用调用的 C 接口。

| 目录 | 内容 |
| --- | --- |
| [zxs-server](zxs-server/) | Linux 存储、HTTP 服务和离线运维 |
| [zxs-client](zxs-client/) | 同步 Rust 内核、桌面 CLI、浏览器界面与移动端 |
| [tools/zxs-prepare](tools/zxs-prepare/) | 独立准备空盘或已有数据盘，提供 Linux、macOS 和 Windows 本机浏览器界面与 CLI |
| [tests/smoke.py](tests/smoke.py) | 同时启动服务端和客户端的端到端测试 |
| [tests/browser.mjs](tests/browser.mjs) | 本机网页、目录包和会话自动续接的浏览器测试 |

`client-v` 标签发布五个客户端包，支持 Linux x86_64、Windows x64、macOS ARM64、Android ARM64 和 iOS ARM64。`v` 标签在同一批次增加 Linux x86_64 服务端包，包内含 `zxs` 和独立的 `zxs-prepare`。`prepare-v` 标签单独发布磁盘准备工具的 Linux x86_64、Windows x64 和 macOS ARM64 包。下载后可用 `SHA256SUMS` 核对文件，桌面、服务端与工具压缩包中的 `BUILD.txt` 记录版本、提交和目标平台。[MIT 许可证](LICENSE) · [下载发布包](https://github.com/isarmg/zxs-s/releases)

Windows MSI 支持选择安装目录。iOS 提供未签名 IPA，配置有效签名后可安装到真机。Android APK 使用固定发布密钥签名。已安装以往调试签名 APK 的用户需要先卸载旧应用，再安装正式签名版并重新填写服务器配置。此后的正式签名版可原地更新。

版本标签与对应组件的 Cargo 版本一致。发布 Android APK 需要设置 GitHub Actions 仓库 Secret `ZXS_ANDROID_KEYSTORE_BASE64`、`ZXS_ANDROID_KEYSTORE_PASSWORD`、`ZXS_ANDROID_KEY_ALIAS` 和 `ZXS_ANDROID_KEY_PASSWORD`。`ZXS_ANDROID_KEYSTORE_BASE64` 存放发布密钥库文件的 Base64 编码，每次发布沿用同一个密钥库和别名。部署时先升级服务端，再升级客户端，以使用目录和容器的批量查询。

## 从空目录开始

已有文件的磁盘可以先用独立工具逐项选择单文件容器或 ZIP 目录包。空盘也可以用同一工具写入数据盘或回收盘身份。 [磁盘准备工具](tools/zxs-prepare/README.md)

仓库通过 `rust-toolchain.toml` 固定 Rust 1.100.0。服务端和客户端各自锁定依赖和 vendor 源，磁盘工具复用客户端的 vendor。构建需要 C 编译器和链接器。x86-64 默认以 x86-64-v3 为基线。服务端需要 Linux 6.6+。客户端目标为 Linux 6.6+、Apple Silicon macOS 15+ 和 Windows 11 24H2+ 的 MSVC 环境。

下载安装包后，将可执行文件所在目录加入 `PATH`。也可在仓库根目录从源码构建，再将对应的 `target/release` 目录加入 `PATH`。

```sh
(cd zxs-server && cargo build --release --offline --locked)
(cd zxs-client && cargo build --release --offline --locked -p zxs-cli)
(cd tools/zxs-prepare && cargo build --release --offline --locked)
```

先用 `zxs-prepare` 准备数据盘和回收盘，再用 `zxs init` 登记已准备的盘。部署时，每个一级子目录挂载独立的磁盘。测试可以使用普通目录 `disk1` 和 `rec1`，回收与还原同样先复制后切换。服务端默认监听 `0.0.0.0:7738`。可通过 `serve --listen <地址:端口>` 或数据根的 `.zxs/config.toml` 更改监听地址。

```sh
mkdir -p ./zxs-data/disk1 ./zxs-recycle/rec1
zxs-prepare init ./zxs-data/disk1 --kind data
zxs-prepare init ./zxs-recycle/rec1 --kind recycle
printf '%s\n' '替换成你自己的口令' | zxs init \
  --data-dir ./zxs-data --recycle-dir ./zxs-recycle
zxs serve --data-dir ./zxs-data
```

另开终端，登录、提交并查看文件。Windows 默认使用系统桌面下的 `ZXS-S-Workspace`，macOS 和 Linux 默认使用启动时的目录。在网页的「工作区 → 选择工作区」中可选择一个已有目录，应用会记住所选路径。要搬动原工作区时，先关闭客户端与编辑器，用系统移动整个目录（含 `.zxs`），再选择新位置。显式 `--workspace` 优先，尚未保存位置时可用 `ZXS_WORKSPACE` 指定默认值。

```sh
mkdir -p demo-work/disk1/文档
printf '%s\n' 'first version' > demo-work/disk1/文档/notes.txt
printf '%s\n' '替换成你自己的口令' | zxs-cli login
zxs-cli --workspace demo-work commit disk1/文档/notes.txt --tag 笔记
zxs-cli ls /disk1/文档
zxs-cli --workspace demo-work ui
```

`ui` 在 `127.0.0.1` 的随机端口启动，并用一次性地址打开默认浏览器。「文件」页按真实目录浏览，也可按名称或标签搜索全库。选中文件后，可用工具栏管理标签。详情栏可以预览图片或音视频原件，并显示文件大小和历史版本。在历史版本中选择「恢复到工作区」即可取回当前版或旧版，旧版内容按待提交的本地修改处理。

桌面网页、交互式终端与 Android、iOS 应用在上传和下载时显示当前文件的进度及速率。目录提交、同步和相册上传按当前传输的文件更新。移动端可点下载进度右侧的 × 取消当前下载。

本地工作区镜像服务端文件，网页「工作区」列出本地变更和目录。在「文件」页可修改文件和文件夹的名称或目录路径，受影响的干净追踪文件随远端路径移动，遇到本地修改会提示处理。浏览、登录和刷新按需读取元数据。用系统文件管理器在本地工作区对应数据盘下创建文件夹，再从网页「工作区」提交。「提交当前目录」递归提交新增和修改的文件及本地空目录，保留目录层级。首次提交沿用工作区内的目录和文件名。在「工作区」进入目标目录，点击「创建 .zxsignore」即可创建规则文件，再逐层浏览或提交整包。无法创建时，界面会显示原因。新恢复的目录包以完整 `.zip` 名称落盘为文件夹，可从「工作区」打开。

### 工作区与版本

- `checkout <id> [--version <N>] [--to <本地路径>]` 检出文件到工作区。默认镜像根据服务器当前目录和文件名确定恢复位置，自定义检出采用已追踪的本地路径。恢复旧版后，显式 `commit` 才发布新版。已有本地改动时可用 `--backup` 确认备份后恢复。
- `status` 使用大小和纳秒级修改时间检查工作区，也列出新增项、缺失项和被忽略的非法 `.zxsignore`。网页浏览工作区目录时按当前子树扫描。
- `sync` 更新已追踪文件，`sync --folder /disk1/文档` 显式检出并同步所选目录及其子目录。网页可用「同步已追踪」或「同步此目录」。同步保留本地修改，覆盖或移动前核对本地 BLAKE3。
- `refresh` 执行同步后列出本地工作区状态。同步失败或存在逐项错误时，`sync` 和 `refresh` 都返回非零退出码。逐项错误仍保留在各自的输出中。
- 网页「工作区」的「提交当前目录」会复核看起来未改动的已追踪文件及目录包成员，保留原大小和修改时间的编辑也会提交。
- `commit <本地路径>` 给已跟踪的文件追加版本。客户端上传时计算或复用 BLAKE3，并核对服务端返回的正文摘要。首次提交沿用工作区内的相对父目录。位于工作区根目录的文件须通过 `--folder /disk1` 指定目标盘。可用 `--name` 和 `--tag` 设置远端名称和标签。
- 遇到版本冲突或追踪文件的服务器镜像路径已变更时，普通提交先停下并提示。可用 `commit <路径> --on-conflict append` 追加到服务器当前位置的最新版本，`--on-conflict new --name <文件名>` 另存，或 `--on-conflict pull` 先备份再检出远端。备份位置随结果返回。
- `init-bundle <本地目录>` 初始化已登记数据盘下的目录包。缺少 `.zip` 后缀时在原位置改名并创建 `.zxsignore`，已有 `.zip` 或 `.ZIP` 时保留原名。CLI 返回改名后的工作区相对路径。初始化会检查本地和远端重名及追踪项重叠。已有 `.zip` 目录包内不能初始化子目录包。提交时本地目录名缺少后缀会报错。客户端会忽略与独立追踪项重叠的手工 `.zxsignore` 并提示。提交目录包会生成首条目为 `.zxsignore` 的 ZIP/ZIP64，其余条目按 NFC 路径排序。检出更新会保留本地被忽略的文件。
- `mkdir <目录>` 创建空目录，`mv-folder <原目录> <新目录>` 移动整棵目录树，`rmdir <目录>` 删除空目录。加 `--recursive` 会将子树内的容器移入回收站。
- `detail <id>` 查看容器和版本历史。`patch <id>` 可以用 `--folder`、`--name`、`--tags` 或 `--clear-tags` 修改元数据。
- `logout` 清除本机保存的服务器登录信息。
- `recycle ls` 查看回收站，`recycle move <容器 ID>` 回收文件。`restore <回收 ID> --folder /数据盘/目录` 还原到指定的在线数据盘，`--name` 可改名。用 `recycle purge <回收 ID>` 或 `recycle purge --all` 彻底删除在线回收盘上的记录。

### 服务端运维

执行离线命令前先停止 `serve`。所有命令共用 `--data-dir` 和同一把进程锁。回收目录在初始化时绑定。`sync-disks` 只登记有盘身份的新盘，磁盘准备中断后可用 `zxs-prepare resume` 继续。`reset-password` 持锁更新配置文件。`ZXS_DATA_DIR` 和 `ZXS_RECYCLE_DIR` 可以代替路径参数。

```sh
zxs --data-dir /srv/zxs import /mnt/source --folder /disk1/导入
zxs --data-dir /srv/zxs sync-disks
zxs --data-dir /srv/zxs prune --keep-last 5
zxs --data-dir /srv/zxs verify
zxs --data-dir /srv/zxs rebuild-index
printf '%s\n' '替换成你自己的新口令' | zxs --data-dir /srv/zxs reset-password
```

已发布的原件位于 `<数据根>/<数据盘>/<目录>/<容器名称>/vNNNNNN/<提交时的文件名>`，权限为 `0444`。容器目录和各版本目录分别在自己的 `.zxs` JSON 文件中保存元数据。空目录保留到显式删除。回收批次位于 `<回收根>/<回收盘>/<日期>_<回收 ID>/`，批次根的 `.zxs` JSON 文件只记录回收 ID、时间和文件名。桌面网页在还原时要求填写目标目录。

跨数据盘移动、回收和还原先在目标盘暂存并核对各版本的 BLAKE3，再移开源目录并发布目标。跨盘移动和回收还核对索引中的版本清单。跨盘迁移或递归回收目录时，服务端等待已受理的相交写入完成，期间源树拒绝新写入并返回 `423 Locked`。服务端在复制或回收前核对源树与索引中的容器集合，发现差异就保留源树并中止操作。读取和其他路径的写入照常。

启动时通过登记盘的 `.zxs/disk.toml` 识别在线盘。服务端扫描一次在线盘并重建 SQLite 索引。没有在途日志时，提交重建结果前会比对旧索引中的在线版本和回收项。复制中断且未写在途日志时，遗留的暂存直接清理。目标已发布时，源盘离线也能结束事务。恢复完成后清理在线盘的 `.zxs/purging/`，源盘普通目录里的文件留在原处。尚未发布且需要离线盘的事务会保留日志与暂存，接回所需磁盘后重启。已登记的盘接回后，重启即可索引其文件。

每个容器记录首个保留版本和最新版。巡检、重建和还原在该范围内检查缺失版本，即使 SQLite 索引已丢失。`verify` 校验在线数据盘和回收盘，并报告离线盘。巡检和重建会报告缺少描述文件的回收批次。

升级前，先让旧版服务端完成在途操作并停服。将存量容器、版本和回收批次的元数据整理为各自目录下的 `.zxs` JSON 文件，再启动新版。[磁盘布局](zxs-design-document.md)

## HTTP/1.1 契约

受保护请求在请求头中使用 `Authorization: Bearer <token>`。客户端安全保存服务端地址和密码，服务端重启后自动重新登录。执行 `reset-password` 后需要使用新密码登录。上传成功却丢失响应时，再次提交会核对远端最新内容的 BLAKE3，相同内容会直接认领。

| 方法和路径 | 请求与结果 |
| --- | --- |
| `POST /sessions` | JSON `{"password":"…"}`。返回 `201` 和 `token`，会话保存在服务进程内存中。 |
| `DELETE /sessions/current` | 撤销当前会话，返回 `204`。 |
| `POST /containers` | 上传首版。上传参数为 `folder`、`name`、可选的 `tags`。返回 `201` 和 `{container, version}`。 |
| `POST /containers/{id}/versions` | 上传参数为 `expected_version`。返回 `201` 和 `{container, version}`。 |
| `PATCH /containers/{id}` | JSON `folder`、`name`、`tags` 均可选。返回更新后的容器。 |
| `GET /containers` | 支持 `folder`、`recursive`、`tag`、`q`、`cursor`、`limit`。指定 `folder` 后可用 `recursive=true` 查询该目录及其子目录。返回 `containers` 和 `next_cursor`，容器项含 `latest_blake3`。也可单独传入逗号分隔的 `ids` 批量读取 100 个以内的容器摘要。 |
| `GET /containers/{id}` | 返回 `container` 和升序的 `versions`。 |
| `GET` 或 `HEAD /containers/{id}/versions/{version}/payload` | 完整原件或单区间 `Range`，附强 BLAKE3 `ETag`。区间无效返回 `416`。 |
| `DELETE /containers/{id}` | 将容器移入回收站，返回 `{recycle, recycle_disk, container}`，其中 `container` 省略原目录。可附 `expected_version` 校验版本。 |
| `GET /recycle` | 支持 `cursor`、`limit`，返回在线回收盘上的 `entries` 和 `next_cursor`。 |
| `POST /recycle/{id}/restore` | JSON 必填 `folder`，可选 `name`，默认沿用回收项文件名。返回还原后的容器。 |
| `DELETE /recycle/{id}` 或 `DELETE /recycle` | 彻底删除一条回收记录或清空当前在线回收盘。 |
| `GET /folders?folder=<路径>` | 返回直接子目录，包含空目录。根目录列出在线数据盘。带 `recursive=true` 时返回该路径下的目录树。 |
| `POST /folders` 或 `PATCH /folders` | 传 `{folder}` 创建目录，或传 `{from,to}` 移动目录。 |
| `DELETE /folders?folder=<路径>` | 删除空目录。带 `recursive=true` 时将其中的容器全部回收。 |

上传正文是 `application/octet-stream`。`Zxs-Params` 是无填充 Base64URL 编码的 UTF-8 JSON。`Content-Length` 声明字节数，可选的 `Zxs-Blake3` 声明正文哈希。空文件以 `Content-Length: 0` 正常上传。服务端支持 `Expect: 100-continue` 预检。

## 移动端

- iOS 17+。在 Apple Silicon Mac 上安装 `aarch64-apple-ios` 和 `aarch64-apple-ios-sim` Rust 目标，打开 `zxs-client/apps/ios/ZXS.xcodeproj`。真机运行需在 Xcode 中设置签名团队。
- Android 13+。安装 Android SDK 35、Build Tools 35.0.0、NDK `27.2.12479018`、CMake `3.22.1`、JDK 17、Gradle 8.9 和 Rust 的 `aarch64-linux-android` 目标。用 Android Studio 打开 `zxs-client/apps/android`，或在该目录运行 `gradle :app:assembleDebug`。
- 手机列表先读目录和文件元数据。打开 ZIP 时下载完整原件并校验长度和 BLAKE3。重新打开时核对本机原件缓存的 BLAKE3。已打开的 ZIP 工作区复用本机目录索引，断网时仍可浏览、校验和解压所选成员。普通音视频通过系统播放器流播，文档通过系统预览打开。
- 进入数据盘或其子目录后，点右下角「上传」。选择「上传文件」可从系统选择器单选或多选文件，包括 ZIP，确认后上传到当前目录。选择「上传照片和视频」可从系统相册选择原件，选完后上传到当前目录。
- 在文件的更多菜单中选择「上传新版本」，确认选中的文件和当前版本后提交。批次显示进度及每项结果，遇到冲突时在结果列表选择处理方式，失败项可单独重试。

移动端原件和 ZIP 成员各有本机缓存，常规上限分别为 64 份和 512 MiB。iOS 用 Keychain 保存登录凭据，Android 用系统 Keystore 管理的 AES-256-GCM 密钥加密保存。服务端重启后应用使用已保存的口令自动续接。

## 验证和依赖更新

CI 会为服务端、客户端和磁盘准备工具执行 `cargo clippy --all-targets --all-features -- -D clippy::pedantic -D warnings`。

从仓库根目录运行 `python3 tests/run.py`。可用 `--suite` 选择 Rust、集成、原生桌面、Chromium 或 Android 测试。Linux 可用 `--suite integration --recycle-parent /dev/shm` 加跑跨文件系统回收和还原。测试矩阵、平台前置条件与单项复跑命令见 [测试说明](tests/README.md)。

两个 `tools/sync_vendor.py --check` 分别核对依赖清单，需 Python 3.11+。更新依赖时在各项目更新 `Cargo.lock`，再运行自己的 `tools/sync_vendor.py`。

Linux 上可另用 Node.js 22 运行真实 Chromium 测试。Playwright 只用于测试，桌面客户端的网页仍由 Rust 二进制直接提供。

```sh
cd tests
npm ci --ignore-scripts
npx --no-install playwright install --with-deps --only-shell chromium
npm test
```

吞吐、冷启动和 4K/8K 播放目标需要在目标网卡、NVMe 和终端上实测。
