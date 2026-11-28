# ZXS 运维、命令行、构建与协议手册

本文档收录 ZXS 的完整命令行用法、服务端离线维护流程、全平台源码离线构建、发行签名说明及 HTTP/1.1 通信契约。快速上手与磁盘布局概览请参见根目录 [README](../README.md)，底层事务恢复与架构约束详见 [系统设计规格书](../zxs-design-document.md)。

---

## 一、桌面客户端与工作区命令（`zxs-cli`）

### 1. 工作区、本机配置与移动端缓存位置

- **工作区默认路径**：Windows 默认使用系统桌面下的 `ZXS-S-Workspace`；macOS 和 Linux 默认使用启动时的当前目录。
- **切换与搬移**：可在 `zxs-cli ui` 的「工作区 → 选择工作区」中选定任意已有目录并持久保存。命令行显式传入 `--workspace <目录>` 优先级最高；尚未保存工作区位置时可用环境变量 `ZXS_WORKSPACE` 指定默认值。搬移工作区时先关闭客户端与编辑器，用系统文件管理器移动含 `.zxs/` 在内的整个目录，再重新选择新位置。
- **桌面本机配置目录**：Linux `~/.config/zxs/`（或 `$XDG_CONFIG_HOME/zxs`）· macOS `~/Library/Application Support/ZXS/` · Windows `%APPDATA%\ZXS\`（可用 `ZXS_CONFIG_DIR` 覆盖），存放 `profile.json`（`0600` 权限）与 `workspace-location.json`。
- **移动端凭据与缓存**：iOS 使用 Keychain、Android 使用系统 Keystore（AES-256-GCM）保存凭据；应用私有 `cache/documents/` 与 `members/` 分别缓存原件与解压的 ZIP 成员，常规上限各为 64 份 / 512 MiB，重开时校验长度与 BLAKE3。已安装早期调试签名版 Android APK 的设备，首次切换正式版前需先卸载旧应用。

### 2. 常用子命令速查

| 命令 | 行为与关键选项 |
| --- | --- |
| `login [http://host:port]` / `logout` | 验证并保存服务器地址与口令（默认 `http://127.0.0.1:7738`，支持标准输入管道或 `ZXS_PASSWORD` 传口令）；`logout` 清除本机保存的登录凭据。 |
| `ui` | 在 `127.0.0.1` 随机端口启动本机管理网页并以一次性令牌打开默认浏览器，提供文件浏览、全库搜索、标签编辑、历史恢复、工作区提交与回收站管理。 |
| `ls [/盘/目录] [--q <词>] [--tag <标签>]` | 指定目录时列出该目录下的文件与直接子目录；省略目录时跨全盘检索容器。 |
| `checkout <id> [--version <N>] [--to <本地路径>] [--backup]` | 检出容器到工作区。默认按服务端当前路径落盘，已自定义路径的追踪项沿用原本地路径。恢复旧版后按本地待提交修改保留，显式 `commit` 才发布新版；本地有未提交改动时加 `--backup` 先备份再恢复。 |
| `status` | 利用文件大小与纳秒级修改时间快速扫描工作区，输出新增项、修改项、缺失项、中断检出项及被忽略的非法 `.zxsignore` / 符号链接提示。 |
| `sync [--folder /盘/目录]` / `refresh` | `sync` 批量更新已追踪文件，加 `--folder` 递归检出并同步指定目录树（覆盖或跟随远端移动前强制核对本地 BLAKE3，保留本地修改）；`refresh` 在同步后输出最新工作区状态。存在失败项时均返回非零退出码。 |
| `commit <本地路径> [--folder /盘] [--name <名>] [--tag <签>]` | 提交新增文件或为已追踪文件追加新版本（核对服务端回执 BLAKE3）。首次提交默认沿用工作区内的相对父目录；直接放在工作区根目录的文件须用 `--folder /disk1` 指定目标数据盘。网页「提交当前目录」还会复核大小与修改时间未变的追踪项，确保等长覆盖编辑不漏提。 |
| `commit <路径> --on-conflict <策略>` | 遇到远端版本推进或追踪路径变更时默认暂停提示：可用 `append` 追加到远端当前位置最新版、`new --name <新文件名>` 另存为新容器，或 `pull` 自动备份本地改动后拉取远端最新版。 |
| `init-bundle [本地目录]` | 在已登记数据盘下将目录初始化为 ZIP 目录包：缺少 `.zip` 后缀时原地补齐并写入 `.zxsignore`（已有 `.zip`/`.ZIP` 保留原拼写），返回改名后的工作区相对路径。禁止在已有目录包内嵌套创建，忽略与独立追踪项重叠的手工 `.zxsignore`。 |
| `sync-delete <本地路径> [--expected-version <N>]` / `untrack <本地路径>` | `sync-delete` 带版本保护将本地已删追踪项对应的远端容器移入回收站；`untrack` 仅解除本机追踪记录（如远端已删时保留本地原文件）。 |
| `mkdir <目录>` / `mv-folder <原> <新>` / `rmdir <目录> [--recursive]` | 创建空目录、移动目录树（受影响的干净追踪文件随远端路径自动迁移）、删除空目录（加 `--recursive` 将子树内所有容器移入回收站）。 |
| `detail <id>` / `patch <id>` | `detail` 查看容器详情与历史版本；`patch` 通过 `--folder`、`--name`、`--tags` 或 `--clear-tags` 修改元数据。 |
| `recycle ls` / `recycle move <id>` / `restore <回收ID> --folder <目录> [--name <名>]` / `recycle purge <回收ID \| --all>` | 查看在线回收盘记录、将容器移入回收站、还原到指定在线数据盘目录，或彻底清理单条/全部在线回收记录。 |

---

## 二、服务端离线运维与存储恢复（`zxs`）

执行任何离线维护子命令前须先停止正在运行的 `zxs serve`。所有命令共用 `--data-dir`（或环境变量 `ZXS_DATA_DIR`、`ZXS_RECYCLE_DIR`）与全局排他文件锁 `<DATA_DIR>/.zxs/.lock`。

```sh
zxs --data-dir /srv/zxs import /mnt/source --folder /disk1/导入
zxs --data-dir /srv/zxs sync-disks
zxs --data-dir /srv/zxs prune --keep-last 5
zxs --data-dir /srv/zxs verify
zxs --data-dir /srv/zxs rebuild-index
printf '%s\n' '替换成你的新口令' | zxs --data-dir /srv/zxs reset-password
```

- **磁盘识别与增量接入**：服务端启动时读取已登记盘的 `.zxs/disk.toml` 识别在线盘并扫描重建 SQLite 索引；未接回的盘保持离线跳过，接回后重启即可恢复索引。新增由 `zxs-prepare` 准备完毕的磁盘后，停服运行 `sync-disks` 登记（准备中断可先运行 `zxs-prepare resume`）。
- **跨盘原子切换与并发保护**：跨数据盘移动、回收和还原均先在目标盘暂存并逐版校验 BLAKE3，再移开源目录并原子发布目标。跨盘迁移或递归回收目录期间，服务端等待已受理的相交写入完成，并对源树的新写入返回 `423 Locked`，其余路径读写不受影响。
- **崩溃恢复与巡检**：无在途日志时，重建索引提交前会与旧索引核对在线版本与回收项；尚未发布且依赖离线盘的在途事务会保留日志与暂存，待原盘接回重启后继续完成。`verify` 核对所有在线数据盘与回收盘每版原件的长度和 BLAKE3，并报告离线盘或缺失元文件的目录。
- **版本升级流程**：部署新版本时遵循**先升级服务端、再升级客户端**。升级服务端前先让旧进程完成在途操作并正常停服；若跨布局升级，确认存量容器、版本和回收批次元数据均为各自目录下的 `.zxs` JSON 文件后再启动新版。

---

## 三、从源码离线构建与发版签名

### 1. 构建基线与桌面/服务端编译

仓库通过 `rust-toolchain.toml` 锁定 **Rust 1.100.0**，服务端与客户端各自锁定离线 `vendor/`（`zxs-prepare` 复用客户端 `vendor/`），全程无需联网下载 crate。需要本机 C 编译器与链接器；`x86_64` 默认启用 `x86-64-v3` 基线。目标系统基线：服务端 Linux 6.6+；桌面端 Linux 6.6+、macOS 15+（Apple Silicon）、Windows 11 24H2+（MSVC）。

```sh
(cd zxs-server && cargo build --release --offline --locked)
(cd zxs-client && cargo build --release --offline --locked -p zxs-cli)
(cd tools/zxs-prepare && cargo build --release --offline --locked)
```

### 2. 移动客户端构建

- **iOS 17+**：在 Apple Silicon Mac 上执行 `rustup target add aarch64-apple-ios aarch64-apple-ios-sim`，用 Xcode 打开 `zxs-client/apps/ios/ZXS.xcodeproj`（真机运行需配置签名团队）。
- **Android 13+**：安装 Android SDK 35、Build Tools `35.0.0`、NDK `27.2.12479018`、CMake `3.22.1`、JDK 17、Gradle 8.9 及 `rustup target add aarch64-linux-android`，使用 Android Studio 打开 `zxs-client/apps/android` 或在该目录执行 `gradle :app:assembleDebug`。

### 3. 依赖校验、测试与发行标签

- **依赖同步**：需 Python 3.11+。运行各自目录下的 `python3 tools/sync_vendor.py --check` 校验离线依赖；升级依赖时先更新 `Cargo.lock` 再运行各自的 `tools/sync_vendor.py`。
- **测试执行**：根目录运行 `python3 tests/run.py`（支持 `--suite rust|integration|desktop|browser|android`），详见 [测试说明](../tests/README.md)。CI 对全部工程强制执行 `cargo clippy --all-targets --all-features -- -D clippy::pedantic -D warnings`。
- **发行标签与签名**：
  - 标签须与对应 `Cargo.toml` 版本一致：`v*` 发布 Linux 服务端包（内含 `zxs-prepare`）与全套 5 个客户端包；`client-v*` 仅发布 5 个客户端包；`prepare-v*` 独立发布三平台磁盘准备工具包。桌面与服务端归档内含 `BUILD.txt` 记录版本、提交与编译器信息。
  - GitHub Actions 构建正式签名 Android APK 需配置仓库 Secrets：`ZXS_ANDROID_KEYSTORE_BASE64`（签名密钥库 Base64）、`ZXS_ANDROID_KEYSTORE_PASSWORD`、`ZXS_ANDROID_KEY_ALIAS` 与 `ZXS_ANDROID_KEY_PASSWORD`，每次发布沿用同一密钥库与别名以支持覆盖升级。

---

## 四、HTTP/1.1 通信契约速查

除 `POST /sessions` 外，所有请求均通过 `Authorization: Bearer <token>` 鉴权（禁止 URL 传参）。服务端仅在进程内存中保存令牌摘要；重启后客户端收到 `401` 会使用本机安全存储的口令自动重登续接，仅在执行 `reset-password` 或主动登出后需重新输入口令。

流式上传请求体为 `application/octet-stream`，必带 `Content-Length`（支持 `0` 字节空文件）与 `Zxs-Params`（无填充 Base64URL 编码的 UTF-8 JSON），可选附带 `Zxs-Blake3` 声明正文哈希，支持 `Expect: 100-continue` 预检。上传成功但响应丢失时，客户端重试核对远端最新 `latest_blake3`，内容一致直接认领。

| 方法与路径 | 请求参数与响应摘要 |
| --- | --- |
| `POST /sessions` | JSON `{"password":"…"}` → `201` 返回内存会话 `token`。 |
| `DELETE /sessions/current` | 销毁当前内存会话 → `204`。 |
| `POST /containers` | 上传首版（`Zxs-Params`: `folder`、`name`、可选 `tags`）→ `201` 返回 `{container, version}`。 |
| `POST /containers/{id}/versions` | 追加新版（`Zxs-Params`: `expected_version`、可选 `expected_path`）→ `201` 返回 `{container, version}`。 |
| `PATCH /containers/{id}` | JSON 可选 `folder`、`name`、`tags` → 返回更新后的容器摘要。 |
| `GET /containers` | 支持 `folder`、`recursive`、`tag`、`q`、`cursor`、`limit` 分页查询（含 `latest_blake3`）；或单独传逗号分隔 `ids` 批量查询最多 100 项。 |
| `GET /containers/{id}` | 返回 `container` 与升序排列的 `versions` 列表（含每版 `size` 与 `blake3`）。 |
| `GET` \| `HEAD /containers/{id}/versions/{version}/payload` | 下载完整原件或单区间 `Range` 流播，附强 BLAKE3 `ETag`（非法区间返 `416`）。 |
| `DELETE /containers/{id}` | 移入回收站（可选 `expected_version`）→ 返回 `{recycle, recycle_disk, container}`。 |
| `GET /recycle` | 支持 `cursor`、`limit`，返回当前在线回收盘上的 `entries` 与 `next_cursor`。 |
| `POST /recycle/{id}/restore` | JSON 必填 `folder`、可选 `name` → 返回还原后的容器摘要。 |
| `DELETE /recycle/{id}` \| `DELETE /recycle` | 彻底删除单条回收记录，或清空当前所有在线回收盘。 |
| `GET` \| `POST` \| `PATCH` \| `DELETE /folders` | `GET ?folder=<路径>[&recursive=true]` 列目录（`/` 列在线数据盘）；`POST {folder}` 创建；`PATCH {from,to}` 移动；`DELETE ?folder=<路径>[&recursive=true]` 删除空目录或递归回收子树。 |
