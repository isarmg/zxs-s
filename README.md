# ZXS

ZXS 是面向局域网的文件和版本管理系统。Linux 服务端把每个版本保留为可以直接打开的原件。同步客户端提供命令行、本机浏览器界面，以及供 iOS 和 Android 原生应用调用的 C 接口。

| 目录 | 内容 |
| --- | --- |
| [zxs-server](zxs-server/) | Linux 存储、HTTP 服务和离线运维 |
| [zxs-client](zxs-client/) | 同步 Rust 内核、桌面 CLI、浏览器界面与移动端 |
| [tests/smoke.py](tests/smoke.py) | 同时启动服务端和客户端的端到端测试 |

## 从空目录开始

两个 Rust 项目各自锁定依赖和 vendor 源。需要 Rust 1.89+、C 编译器和链接器。x86-64 默认以 x86-64-v3 为基线。服务端需要 Linux 6.6+。客户端目标为 Linux 6.6+、Apple Silicon macOS 15+ 和 Windows 11 24H2+ 的 MSVC 环境。

在仓库根目录分别构建。

```sh
(cd zxs-server && cargo build --release --offline --locked)
(cd zxs-client && cargo build --release --offline --locked -p zxs-cli)
```

初始化两个分离的目录，再启动服务。以下示例用于同一台机器演示。要供其他局域网设备访问，在数据根的 `.system/config.toml` 中把 `listen` 设为机器的实际局域网 IP 和端口。

```sh
printf '%s\n' '替换成你自己的口令' | ./zxs-server/target/release/zxs init \
  --data-dir ./zxs-data --recycle-dir ./zxs-recycle
./zxs-server/target/release/zxs serve --data-dir ./zxs-data
```

另开终端，登录、提交并查看文件。`--workspace` 参数可由 `ZXS_WORKSPACE` 环境变量代替。

```sh
mkdir -p demo-work
printf '%s\n' 'first version' > demo-work/notes.txt
printf '%s\n' '替换成你自己的口令' | ./zxs-client/target/release/zxs-cli login
./zxs-client/target/release/zxs-cli --workspace demo-work commit notes.txt \
  --folder /文档 --tag 笔记
./zxs-client/target/release/zxs-cli ls /文档
./zxs-client/target/release/zxs-cli --workspace demo-work ui
```

`ui` 在 `127.0.0.1` 的随机端口启动，并用一次性地址打开默认浏览器。也可从终端复制该地址。界面可以检出并打开本机关联程序、查看历史版本、提交修改、预览媒体和还原回收文件。

### 工作区与版本

- `checkout <id> [--version <N>] [--to <本地路径>]` 检出并登记文件。显式传入 `--backup` 可在覆盖本地改动前保存备份。
- `status` 使用大小和纳秒级修改时间检查工作区。再次提交时会先查未确认提交的回执。
- `commit <本地路径>` 给已跟踪的文件追加版本。首次提交用 `--folder`、`--name` 和 `--tag` 设置远端信息。
- 遇到版本冲突时，可用 `commit <路径> --on-conflict append` 追加到最新版本，`--on-conflict new --name <文件名>` 另存，或 `--on-conflict pull` 先备份再检出远端。备份位置随结果返回。
- `init-bundle <本地目录>` 创建 `.zxsignore`。提交该目录会生成标准 ZIP/ZIP64，按 UTF-8 路径排序，时间固定为 ZIP 支持的最早日期。检出更新会保留本地被忽略的文件，并核对旧跟踪清单。ZIP 封装使用 `zip`，压缩使用 `zlib-rs` Level 1。
- `recycle [<id>]` 列表或回收文件。`restore <回收 ID>` 还原，支持指定新目录和名称。

### 服务端运维

执行离线命令前先停止 `serve`。所有命令共用 `--data-dir` 和同一把进程锁。回收目录在初始化时绑定。`ZXS_DATA_DIR` 和 `ZXS_RECYCLE_DIR` 可以代替路径参数。

```sh
zxs --data-dir /srv/zxs import /mnt/source --folder /导入
zxs --data-dir /srv/zxs prune --keep-last 5
zxs --data-dir /srv/zxs verify
zxs --data-dir /srv/zxs rebuild-index
printf '%s\n' '替换成你自己的新口令' | zxs --data-dir /srv/zxs reset-password
```

已发布的原件在 `containers/<分片>/<容器 ID>/versions/vNNNNNN/`，对应的 `version.json` 保存原始名称、大小和 BLAKE3。服务端在 `.system/inflight` 保存可重放的变更意图，另外登记未完成的上传，并保留成功操作的回执以供重建索引。SQLite 丢失时，服务启动会从数据根和回收根重建索引。跨文件系统回收和还原会先复制到目标盘、同步并发布，再移除源目录。`reset-password` 会让现有会话失效。

## HTTP/1.1 契约

写请求使用 `Zxs-Op-Id`，值为 32 位小写十六进制。成功写入后，相同操作 ID 和请求摘要再次提交会返回相同回执。更改已使用的操作 ID 所对应的参数会收到 `422`。受保护请求使用 `Authorization: Bearer <token>`。播放器可在原件路径上使用 `?token=<token>`。空口令或不合规路径会收到 `400`，鉴权失败为 `401`，目标缺失为 `404`，重名和版本冲突为 `409`。HTTP 默认允许回环、私有、链路本地和组网地址。公网或通配监听需要显式启用 `--allow-public-http`。

| 方法和路径 | 请求与结果 |
| --- | --- |
| `POST /sessions` | JSON `{"password":"…"}`。返回 `201` 和 `token`、`expires_at`，会话有效期 30 天。 |
| `DELETE /sessions/current` | 撤销当前会话，返回 `204`。 |
| `POST /containers` | 上传首版。上传参数为 `folder`、`name`、可选的 `tags`。返回 `201` 和 `{container, version}`。 |
| `POST /containers/{id}/versions` | 上传参数为 `expected_version` 和可选的 `revert_to_version`。返回 `201` 和 `{container, version}`。 |
| `PATCH /containers/{id}` | JSON `folder`、`name`、`tags` 均可选。返回更新后的容器。 |
| `GET /containers` | 支持 `folder`、`tag`、`q`、`cursor`、`limit`。返回 `containers`、`folders`、`next_cursor`。提供 `folder` 时同时返回符合筛选条件的直接子目录。 |
| `GET /containers/{id}` | 返回 `container` 和升序的 `versions`。 |
| `GET /ops/{op_id}` | 返回 `{op_id, status, result}`。没有已生效的操作时返回 `404`。 |
| `GET` 或 `HEAD /containers/{id}/versions/{version}/payload` | 完整原件或单区间 `Range`，附强 BLAKE3 `ETag`。区间无效返回 `416`。 |
| `DELETE /containers/{id}` | 将容器移入回收站，返回 `{recycle, container}`。 |
| `GET /recycle` | 支持 `cursor`、`limit`，返回 `entries` 和 `next_cursor`。 |
| `POST /recycle/{id}/restore` | JSON `folder` 和 `name` 均可选。返回还原后的容器。 |

上传正文是 `application/octet-stream`。`Zxs-Params` 是无填充 Base64URL 编码的 UTF-8 JSON。`Zxs-Blake3` 声明正文的 64 位小写十六进制 BLAKE3，`Content-Length` 声明字节数。回滚上传使用空正文、长度 `0` 和空正文的 BLAKE3。客户端使用 `Expect: 100-continue` 等待服务端预检。

## 移动端

- iOS 17+。在 Apple Silicon Mac 上安装 `aarch64-apple-ios` 和 `aarch64-apple-ios-sim` Rust 目标，打开 `zxs-client/apps/ios/ZXS.xcodeproj`。真机运行需在 Xcode 中设置签名团队。
- Android 13+。安装 Android SDK 35、Build Tools 35.0.0、NDK `27.2.12479018`、CMake `3.22.1`、JDK 17、Gradle 8.9 和 Rust 的 `aarch64-linux-android` 目标。用 Android Studio 打开 `zxs-client/apps/android`，或在该目录运行 `gradle :app:assembleDebug`。
- 两个应用都通过系统相册选择器备份选中的原件到 `/相册`，通过系统播放器流播，并通过系统预览功能查看下载的文档。

## 验证和依赖更新

CI 会在两个 Rust 工程分别执行 `cargo clippy --all-targets --all-features -- -D clippy::pedantic -D warnings`。

在两个项目各自运行 `cargo test --offline --locked --workspace`。构建调试二进制后，从仓库根运行 `python3 tests/smoke.py`。Linux 上可加 `ZXS_SMOKE_RECYCLE_PARENT=/dev/shm` 验证跨文件系统回收和还原。两个 `tools/sync_vendor.py --check` 分别核对依赖清单，需 Python 3.11+。更新依赖时在各项目更新 `Cargo.lock`，再运行自己的 `tools/sync_vendor.py`。

吞吐、冷启动和 4K/8K 播放目标需要在目标网卡、NVMe 和终端上实测。
