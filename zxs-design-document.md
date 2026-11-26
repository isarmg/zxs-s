# ZXS 总体架构与系统设计规格书

> **文档定位**：本文是 ZXS 从零全新实现的唯一总体工程规格书。面向**个人超高速局域网（10GbE / 25GbE / 40GbE / 雷电直连）+ 全 NVMe 固态存储 + 纯现代 64 位软硬件栈**，管理**办公文档、照片、视频、代码/文本项目、普通文件**。全系统拆分为 **`zxs-server`（仅现代 Linux）与 `zxs-client`（跨桌面与移动端）两个独立子项目**，依赖互不相通；**不做历史兼容、不搞协议版本号、不写特殊分支处理**，协议变更时两端同步升级。

---

## 一、目标基线、设计原则与反过度设计红线

### 1.1 现代软硬件基线（零兼容包袱）

| 维度 | 支持基线 | 工程收益 |
| :--- | :--- | :--- |
| **CPU 指令集** | **x86_64**：`x86-64-v3`（AVX2 + BMI2 + FMA + PCLMULQDQ，自建可开 `target-cpu=native`）<br>**ARM64**：Apple Silicon（M1+）/ ARMv8.2-A+（NEON + 硬件 CRC32） | 默认全开 SIMD 向量化，BLAKE3 哈希与客户端 ZIP 压缩无标量慢速回退；不构建任何 32 位或旧架构产物。 |
| **服务端 OS** | **仅现代 Linux 6.6+ LTS**（`x86_64` / `aarch64`） | POSIX 语义一致（打开流读取期间可直接原子重命名或移动目录）；原生支持在 `--data-dir` 与 `--recycle-dir` 下按一级子目录直接挂载多块独立物理 NVMe（拔下任意单盘均可独立只读直取）。 |
| **桌面客户端 OS** | **Linux 6.6+** · **macOS 15+**（Apple Silicon） · **Windows 11 24H2+**（`x86_64-msvc`） | 单一纯同步客户端二进制（含 CLI 与本机 `127.0.0.1` 浏览器 UI），零重型 GUI 框架依赖。 |
| **移动客户端 OS** | **iOS 17+**（`aarch64-apple-ios`） · **Android 13+**（`aarch64-linux-android`） | 通过约 200 行零依赖 C-ABI（`crates/ffi`）接入轻量原生壳，覆盖相册备份、媒体流播与文档查看。 |
| **语言基线** | **Rust Edition 2024**（稳定版 1.89+） | 原生支持 `LazyLock`、`File::try_lock` 等现代标准库能力，淘汰过渡期胶水包。 |

### 1.2 五条核心原则

1. **双项目独立、同步升级与防呆标记**：`zxs-server` 与 `zxs-client` 代码与 `vendor/` 互不相通；**不设 URL 版本号、不做旧版兼容或自动迁移代码**，变更时两端一并升级；在 `.zxs/config.toml` 写入常量 `layout = 1` 作为布局防呆保险丝（不匹配直接拒启报错）。
2. **双根目录各挂 N 块物理磁盘、真实目录树与只读裸盘直读**：服务端指定一个 `--data-dir` 与一个 `--recycle-dir`，**两者内部的一级子目录分别代表挂载的 N 块数据物理 NVMe 与 M 块回收物理 NVMe**（每块盘内写入 `.zxs/disk.toml` 身份标记防漏挂载写穿根分区，并拥有本盘专属 `.zxs/` 暂存工作区）；数据盘一级目录直接呈现为根 `"/"` 下的顶层目录，盘内按**用户真实的目录层级**组织（完整保留空文件夹），每个文件对应一个**以真实文件名命名的容器目录**（**严禁使用 UUID/哈希乱码目录名**），内部按 `v000001`、`v000002` 递增存放**只读（`0444`）真实文件名原件**（拔下任意单块物理 NVMe 即可独立只读直取完整目录树，严禁原地修改），容器元数据不冗余保存路径与文件名（同盘非空文件夹整树移动仅为单次 `rename`，零子孙 `container.json` 重写）；全局统一由单个 `index.sqlite` 提供跨全盘毫秒级检索且随时可从裸盘并行重建。
3. **本盘就近暂存 + 统一“先无锁落盘、再短暂写锁切换”模式**：上传直接在**目标数据盘自身的 `.zxs/staging/`** 内完成，发布永远是同盘 `0.1ms` 原子 `rename`；跨数据盘移动（单容器或整棵文件夹树）、跨盘移入回收站与跨盘还原全部统一为**无锁流式预拷至目标盘暂存区 → `sync_all` → 短暂写锁核对并各盘本盘原子 `rename` 切换**，盘满或断连由 `StagingGuard::drop` 自动清理且源盘丝毫无损，零回滚负担、零断点续传池、零幂等回执账本。
4. **单调线性版本 + 多回收盘按剩余空间自动选盘**：单线递增版本号（`prune` 清理旧历史版本但必留最新版，允许历史稀疏，新版本号永远为 `latest_version + 1`）；删除容器时自动按 `statvfs` 实时可用空间选定剩余容量最大的回收物理盘，平铺迁入 `<RECYCLE_DIR>/<回收盘>/<YYYY-MM-DD>_<recycle_id>/`（保留真实文件名与原路径记录，支持原位或跨盘还原与彻底清理）。
5. **服务端纯粹单文件 + 客户端 `.zxsignore` 整包收发**：服务端不知晓也不解析 `.zip`，不做任何 `.zip` 特殊字段；客户端识别含 `.zxsignore` 的本地目录自动多核打包为标准 `.zip` 整包收发，并在全端界面显示为文件夹。

### 1.3 反过度设计红线（明令禁止项）

* **禁止复杂逻辑卷池化与掉盘降级状态机**：数据物理盘直接映射为根 `"/"` 下可见的一级目录（不搞隐藏跨盘拆散同一文件夹的 MergerFS 式池化路由表、配额账本或掉盘带病读写降级状态机；任一已登记物理盘未挂载直接拒启保护；回收盘仅做 `statvfs` 最大剩余空间自动选盘）。
* **禁止分块断点续传状态机与 `multipart`**：不建断点续传 `uploads/` 暂存池、不维护偏移锁与过期回收线程。
* **禁止重型桌面 GUI 框架（如 Tauri / Electron / egui）与第三方移动 UI 重框架**：桌面交互由 `zxs-cli ui` 托管内嵌零构建 ES2024 页面实现；iOS 用纯 SwiftUI，Android 用纯 SDK 原生 View（零 AndroidX / Material 依赖）。
* **禁止远端 `.zip` 局部拆包读写与服务端缩略图**：`.zxsignore` 目录包上传与检出一律整包高速流式收发；HTTP `Range` 仅用于服务端响应大视频拖动播放，以及客户端列表展示时读取 `.zip` 前 40 字节头部（`bytes=0-39`）作 O(1) 快速预判（检出解包时仍核验完整归档，绝不远端拆包提取正文）；列表视图不生成缩略图。
* **禁止在客户端核心引入异步运行时**：客户端核心统一采用纯同步 I/O + `ureq` 连接池 + `rayon` 线程池。
* **禁止协议版本协商、复杂分支图、后台自动同步守护进程、内置文档编辑器及未到阶段的空壳模块**。

---

## 二、双独立子项目结构与极简依赖治理

### 2.1 目录结构

```text
/
├── README.md                      # 总索引、快速上手与 HTTP 协议契约说明
├── zxs-design-document.md         # 本规格书
├── tests/smoke.py                 # 同时启动服务端（多盘目录）、CLI 与 C-ABI 真连对打的端到端冒烟测试
├── zxs-server/                    # 【子项目 1：Linux 存储服务端单二进制】
│   ├── .cargo/config.toml         # 锁定 ./vendor 离线源与 Linux 现代 CPU 编译参数
│   ├── Cargo.toml                 # 单包工程（forbid(unsafe_code)）
│   ├── Cargo.lock
│   ├── VENDOR.toml                # 上游包版本、许可证与原始包 SHA-256
│   ├── VENDOR.sha256              # 剪裁后 vendor/ 逐文件哈希清单
│   ├── tools/sync_vendor.py       # 独立 vendor 同步与剪裁脚本
│   ├── vendor/                    # 服务端第三方源码（零压缩库、零 GUI 库）
│   └── src/
│       ├── main.rs                # 入口：zxs serve | init | sync-disks | import | prune | verify | rebuild-index | reset-password
│       ├── domain/                # 强类型 ID、一级盘路径边界与 NFC 跨平台路径/保留名校验、错误码（零 I/O）
│       ├── store/                 # layout.rs(唯一路径拼接) + fs_ops.rs + recovery.rs + db.rs(唯一 SQL)
│       ├── service/               # 本盘原子流发布、同盘/跨盘容器与文件夹移动、多盘选盘回收还原、认证、离线运维编排
│       └── api/                   # axum HTTP/1.1 路由、鉴权与 Range 原件流
│
└── zxs-client/                    # 【子项目 2：跨平台客户端套件】
    ├── .cargo/config.toml         # 锁定 ./vendor 离线源与全端现代 CPU 编译参数
    ├── Cargo.toml                 # 纯同步 Workspace（core + cli + ffi）
    ├── Cargo.lock
    ├── VENDOR.toml
    ├── VENDOR.sha256
    ├── tools/sync_vendor.py       # 独立 vendor 同步与剪裁脚本
    ├── vendor/                    # 客户端第三方源码（零异步运行时、零重型 GUI 包）
    ├── crates/
    │   ├── core/                  # 纯同步内核：domain / store(workspace.sqlite) / transport / zip_bundle / service
    │   └── ffi/                   # 约 200 行零依赖 C-ABI + 内容寻址有界文档缓存
    └── apps/
        ├── cli/                   # 单一桌面客户端：完整 CLI + `zxs-cli ui`（内嵌静态原生 ES 模块页面）
        ├── ios/                   # iOS 17+ SwiftUI 薄壳（Keychain + PhotosPicker + AVPlayer + QuickLook）
        └── android/               # Android 13+ 纯 SDK 薄壳（Keystore AES-GCM + PhotoPicker + VideoView + ContentProvider）
```

### 2.2 直接依赖选型与 `vendor/` 剪裁规范

* **`zxs-server` 直接依赖**：`tokio`（`rt-multi-thread, macros, net, io-util, time, signal, sync`，不引入 `tokio/fs`、`tokio-util` 与 `futures-util`，所有磁盘流读写统一走 `spawn_blocking` + 同步 `std::fs::File`）、`axum`（`http1, json, query, tokio`）、`rustix`（`fs`，零 `unsafe` 安全查询各挂载盘 `statvfs` 剩余空间与设备号）、`base64`、`unicode-normalization`、`getrandom`、`rusqlite`（`bundled`）、`blake3`（`rayon`）、`rayon`、`argon2`、`subtle`、`zeroize`、`serde`、`serde_json`、`toml`、`thiserror`、`tracing`、`tracing-subscriber`（`fmt, ansi`）、`clap`（`derive, env`）。
* **`zxs-client` 直接依赖**（纯同步）：`ureq`（关闭默认 TLS，内置线程安全 HTTP/1.1 Keep-Alive 连接池）、`tiny_http`（仅用于 `zxs-cli ui` 监听 `127.0.0.1:0` 本机管理页）、`rustix`（Unix `fs`）与 `winsafe`（Windows `kernel`，零 `unsafe` 查询目标盘剩余空间）、`base64`、`unicode-normalization`、`rusqlite`（`bundled`）、`blake3`（`rayon`）、`getrandom`、`zip`（关闭默认特性，启用 `deflate-flate2-zlib-rs`）、`ignore`、`rayon`、`serde`、`serde_json`、`thiserror`、`clap`（`derive, env`）。
* **`sync_vendor.py` 剪裁规则**：自动删除各包内 `tests/`、`benches/`、`examples/`、`docs/`、`sqlcipher/`、`test-macros/`、`test-data/` 及所有 32 位、Intel Mac、`gnu`/`gnullvm`、BSD/WASI 非目标平台文件；重写 `.cargo-checksum.json` 为 `{"package": "<upstream_sha256>", "files": {}}` 并输出 `VENDOR.sha256`。构建全程强制 `cargo test --offline --locked`。

---

## 三、服务端多盘存储设计 (`zxs-server`，现代 Linux 专属)

### 3.1 统一标识符、多盘路径命名空间与网络访问边界

1. **ID 格式与职责边界**：
   * `container_id` / `recycle_id` / `disk_id` / 内部临时事务号 `tx_id` 为 **32 位小写十六进制字符串**（128-bit，`container_id` 用 UUIDv7 编码）；`session_token` 为 **64 位小写十六进制字符串**（256-bit 随机数）；`version` 为单调递增 `u32`，版本目录名固定为 `v000001`。
   * **`container_id` 仅作为 API 路由、SQLite 主键、客户端跟踪与在途日志的稳定标识，绝不用作数据盘下的磁盘容器目录名。**
2. **跨平台路径归一化、一级物理盘边界、保留名与命名空间规则**：
   * **Unicode NFC 归一化与 5 端合法字符集**：入口统一对所有目录段名（含物理盘挂载目录名）、`name` 及 `.zip` 工作区内部相对路径执行 **Unicode NFC 归一化**（消除 macOS/iOS NFD 与 Windows/Linux NFC 拆字差异）；`folder` 以 `/` 开头、不以 `/` 结尾（根为 `"/"`），归一化后单段名与 `name` 长度为 `1..=255` UTF-8 字节，统一拒绝 `/`、`\`、`\0`、控制字符、`< > : " | ? *`、段尾空格或段尾句点、`.`、`..` 及 Windows 设备保留名（`CON`、`PRN`、`AUX`、`NUL`、`COM1..9`、`LPT1..9`，不区分大小写且含带扩展名形式如 `aux.txt`），确保任一端存入的文件在 5 大操作系统 100% 无损检出；完整逻辑路径（`folder` + `name`）总长 `≤ 1024` UTF-8 字节。
   * **一级目录固定映射物理数据盘挂载点**：根 `"/"` 下的直接一级目录 `"/<数据盘名>"` 严格对应 `<DATA_DIR>/<数据盘名>/` 挂载的物理磁盘（由 `zxs init` 或扩容命令 `zxs sync-disks` 离线登记）；**禁止直接在根 `"/"` 下创建文件容器（`folder == "/"` 返回 `400`），且禁止通过在线 API 新建、重命名、移动或删除一级数据盘目录本身**（作用于 `"/"` 或一级目录 `"/<数据盘名>"` 的目录写操作返回 `400/409`）；在任意已登记数据盘内部（`folder` 为 `"/<数据盘名>"` 或 `"/<数据盘名>/..."`）完整支持任意层级目录与文件容器操作。
   * **全局唯一保留名 `.zxs`**（不区分大小写）：禁止作为任何 `folder` 段名、磁盘挂载目录名或 `name`，彻底隔离系统元数据与任意用户文件（用户上传 `container.json`、`version.json`、`recycle.json`、`disk.toml` 等文件名均零冲突）。
   * **统一大小写不敏感路径命名空间**：普通目录（含一级盘目录与空子目录）与文件容器共享同一棵大小写不敏感树（保留首次创建或显式改名后的显示大小写，防 macOS/Windows 签出冲突）：
     * 同一 `folder` 下禁止同名容器或目录（按 NFC + 小写规范键唯一；容器或子目录自身纯大小写改名除外）；
     * 禁止同一路径既作目录又作容器，禁止并存仅大小写不同的目录（冲突返回 `409` 并提示已有拼写）。
   * **只读真实文件名原件**：每个版本目录内直接以提交时的真实文件名（`name_at_commit`）存放原件（发布时设为 `0444` 只读权限，防止拔盘双击误保存破坏历史原件或同盘硬链接共享版本），**不改名为 `payload.<ext>` 或 `payload.bin`**。容器后续改名时仅重命名容器目录，历史版本内已落盘原件名保持不可变，新提交版本使用新文件名；裸盘直读仅供只读浏览与应急取回，严禁直接原地修改数据盘下文件。
3. **网络监听与传输安全边界**：服务端默认直接开放监听 `0.0.0.0`（可通过 `--listen` 或 `config.toml` 指定端口与地址），客户端全端保持零 TLS 依赖、直连用户配置的 `http://` 服务端地址，不做网段拦截或 `--allow-public-http` 限制；**跨不可信网络或公网访问时，传输加密统一由透明网络层加密组网（WireGuard / Tailscale 等 VPN 隧道）提供**（隧道内依然直连 `http://`）。

### 3.2 多物理盘子目录挂载物理布局（路径即唯一事实源）

`--data-dir` 下包含 `N`（`N >= 1`）个一级文件夹，每个文件夹挂载一块物理数据 NVMe；`--recycle-dir` 下包含 `M`（`M >= 1`）个一级文件夹，每个文件夹挂载一块物理回收 NVMe（单盘环境与测试环境同样只需各建 1 个子目录如 `disk1/` 与 `rec1/`，语义 100% 统一）。拔下任意单块数据 NVMe 即可独立只读直取其上完整目录树；回收盘未挂载时仍可离线只读直取或对数据盘执行 `zxs verify`：

```text
<DATA_DIR>/                                    # 数据根目录（仅存全局配置、锁、在途日志与 SQLite 索引）
├── .zxs/                                      # 全局系统元数据（唯一保留目录名）
│   ├── config.toml                            # layout = 1、密码哈希、监听地址、recycle_dir、已登记 data_disks 与 recycle_disks 清单
│   ├── .lock                                  # File::try_lock() 全局排他锁（在线服务与全部离线运维命令互斥）
│   ├── inflight/<tx_id>.json                  # 发布/移动改名/整树移动/回收/还原全局在途意图日志
│   └── index.sqlite                           # 跨全盘统一 SQLite WAL 加速索引（删掉可从全部挂载盘并行重建）
├── nvme1/                                     # 【物理数据盘 1 挂载点】（逻辑根下呈现为 /nvme1，也可命名为 /工作盘 等）
│   ├── .zxs/                                  # 本物理盘专属系统工作区（确保上传与切盘 100% 本盘原子 rename！）
│   │   ├── disk.toml                          # kind = "data", disk_id（防重启漏挂载写穿系统根分区）
│   │   ├── staging/<tx_id>/                   # 本盘流式上传、跨盘迁入与跨盘还原暂存区
│   │   └── purging/<tx_id>/                   # 本盘回收或跨盘迁出后瞬时摘除源目录中转区（释锁后无锁清盘）
│   ├── 待整理/                                # 空文件夹真实存在，不再自动删除
│   └── 文档/
│       └── 2026/
│           └── 方案.docx/                     # 文件容器目录名 = 真实文件名（无 UUID 乱码）
│               ├── .zxs/
│               │   └── container.json         # 仅存容器固有元数据（不存 folder/name！）
│               ├── v000001/
│               │   ├── .zxs/
│               │   │   └── version.json       # 一旦发布永不修改
│               │   └── 方案.docx              # 0444 只读真实文件名原件（拔下本盘即可直接读取）
│               └── v000002/...
└── nvme2/                                     # 【物理数据盘 2 挂载点】（逻辑路径 /nvme2/...）
    ├── .zxs/
    │   ├── disk.toml
    │   ├── staging/<tx_id>/
    │   └── purging/<tx_id>/
    └── 相册/
        └── 日本旅行/
            └── IMG_0001.JPG/...

<RECYCLE_DIR>/                                 # 回收根目录（内部每个一级文件夹挂载一块物理回收 NVMe）
├── rec1/                                      # 【物理回收盘 1 挂载点】
│   ├── .zxs/
│   │   ├── disk.toml                          # kind = "recycle", disk_id（未挂载时拒绝启动在线服务）
│   │   ├── incoming/<tx_id>/<recycle_id>/     # 迁入本回收盘的无锁预拷贝暂存区（StagingGuard 兜底）
│   │   └── purging/<tx_id>/                   # 彻底删除或还原迁出后的瞬时摘除目录（释锁后无锁清盘）
│   └── <YYYY-MM-DD>_<recycle_id>/             # 平铺单层回收批次目录（拔下本回收盘直观可见，还原后整目录清零）
│       ├── .zxs/recycle.json                  # recycle_id, recycled_at, original_folder, original_name
│       └── 方案.docx/...                      # 完整移入的原文件容器目录
└── rec2/                                      # 【物理回收盘 2 挂载点】（按实时最大剩余空间自动分担回收写入）
    ├── .zxs/
    │   ├── disk.toml
    │   ├── incoming/<tx_id>/<recycle_id>/
    │   └── purging/<tx_id>/
    └── ...
```

* **`.zxs/container.json`（极简 5 字段，不存路径与名称）**：
  `{ "container_id": "...", "tags": ["工作"], "latest_version": 2, "created_at": 1763942400, "updated_at": 1763946000 }`
  * 容器的 `folder`（如 `"/nvme1/文档/2026"`）与 `name` **唯一由其在 `<DATA_DIR>/` 下的真实相对路径决定**，索引在全局 `index.sqlite` 中；重命名或移动任意非空文件夹时，**无论同盘还是跨盘均无需重写任何子孙容器的 `container.json`**！
  * `tags` 为**容器级逻辑标签**（仅通过客户端 Web UI / CLI 添加与筛选，藏于 `.zxs/container.json` 并索引至 SQLite，系统文件浏览器不可见，不随历史版本变化；增删标签不产生新版本，也不中断进行中的上传）。
* **`v000002/.zxs/version.json`（极简 5 字段）**：
  `{ "version": 2, "created_at": 1763946000, "name_at_commit": "方案.docx", "size": 1048576, "blake3": "..." }`（版本目录内原件文件名固定等于 `name_at_commit`，不冗余存储 `payload_file`）。
* **空目录保留规则**：空文件夹是合法的一等目录实体，容器移走、改名或移入回收站后**不自动删除变空的父目录**；删除子目录必须通过显式 `DELETE /folders`。

### 3.3 SQLite 全局索引、多盘原子写路径与故障恢复

* **`index.sqlite`（`WAL + synchronous=FULL + busy_timeout=5s`，1 写 + 8 非阻塞轮询读连接池）**：
  * 仅维护 **`folders`、`containers`、`versions`、`tags`、`recycle_bin` 五张纯业务 `STRICT` 表**（无会话表、无幂等回执表、无隔离表）。
  * `folders` 表记录 `path_key`（NFC + 小写完整目录路径，PK，含各一级数据盘 `"/<盘名>"`）、`parent_key`（小写父目录路径，一级盘之父为 `"/"`，建索引支撑直接子目录字母序列表）与 `path`（显示大小写）；
  * `containers` 表记录 `id`（PK）、`folder`、`folder_key`（小写）、`name`、`name_key`（小写）、`path_key`（小写完整逻辑路径，`UNIQUE`）、`latest_blake3`（供断线重试认领与移动端相册批量去重 1 次请求完成）与 `data`（固有元信息 JSON），建 `(folder_key, name_key, id)` 复合索引支撑跨盘或单目录自然字母序分页；`recycle_bin` 表额外记录所属 `recycle_disk` 名，还原与清理免扫描直达对应回收物理盘；
  * **永不过期的纯内存会话与密码登录机制**：服务端仅在内存中维护已签发的 `session_token`（`token_blake3` 集合，**永不过期**）；客户端将密码永久安全保存在本地（桌面 `0600` 凭证文件 / iOS Keychain / Android Keystore AES-256-GCM），服务端重启后客户端首次遇 `401` 自动用本地密码静默重新登录一次，**仅当服务端通过 `zxs reset-password` 设置新密码后旧密码才真正失效**。
* **本盘就近暂存与流式上传**：
  1. **收流前预检（`Expect: 100-continue`）**：校验目标路径位于有效一级 `<数据盘>` 下且无命名空间冲突、核对 `expected_version`（冲突在读取正文前快速返 `409`；并发同时通过预检的同名新建请求在后进入写锁时同样返 `409` 并清暂存区）；
  2. **目标数据盘本盘无锁流式落盘**：支持上传 `0` 字节合法空文件；常规上传直接在目标容器所属数据盘的 **`<DATA_DIR>/<数据盘>/.zxs/staging/<tx_id>/`**（由 `StagingGuard` 持有）按 **4 MiB 块**读取网络流，交 `spawn_blocking` 同步写入 `<真实文件名>` 并推进多核 `blake3::update_rayon`；流结束严格核对 `Content-Length`（若携带可选头 `Zxs-Blake3` 则强校验一致），设为 `0444` 只读权限，写入 `.zxs/version.json` 并 `sync_all()`（响应返回服务端算出的 `blake3` 供单遍上传客户端核对）；
  3. **加锁本盘原子发布**：持全局单把 `RwLock` 写锁从 SQLite 重读容器当前状态：
     * 若容器已被回收/不存在，返 `404`；若仅 `tags` 在上传期间被修改，正常发布并保留最新 `tags`；
     * 若 `latest_version != expected_version`，或新建目标路径已被并发占用，或容器在上传期间**发生移动（`folder` 变更，含跨盘或父目录整树移动）或改名（`name` 变更）**，直接返 **`409 Conflict`** 并由 `StagingGuard` 自动清理本盘暂存区；
     * 校验通过后：分配 `new_ver = latest_version + 1` → 写全局 `.zxs/inflight/<tx_id>.json` → 首版补齐本盘内祖先目录并创建容器目录 → **本盘瞬间原子 `rename(<数据盘>/.zxs/staging/<tx_id>, <容器目录>/v00000N)`** → 原子更新 `<容器目录>/.zxs/container.json` → 提交 SQLite 事务 → 删 `inflight`。
* **同盘/跨数据盘移动改名、多回收盘自动选盘回收与还原（全场景统一两步法）**：
  * **同数据盘移动或改名**（单容器 `PATCH /containers/{id}` 或子目录整树 `PATCH /folders`，`src_disk == dst_disk`）：持全局 `RwLock` 写锁校验合法性与唯一性（排除自身以支持纯大小写改名；整树移动须 `from`/`to` 均为盘内二级及以上路径、禁止移入自身子树、且子孙路径总长均 `≤ 1024` 字节）→ 写 `inflight/<tx_id>.json` → 补齐目标父目录 → **本盘单次 `rename(from_path, to_path)`（正在流播的打开句柄不受影响，整树移动零子孙 `container.json` 重写）** → 单事务更新 SQLite（整树移动批量改子树 `folders` 与 `containers` 路径前缀）→ 删 `inflight`。
  * **跨物理数据盘移动**（单容器 `PATCH /containers/{id}` 或子目录整树 `PATCH /folders`，`src_disk != dst_disk`）：
    1. **无锁预拷贝**：读锁下拍取源容器（或源目录整棵子树）状态快照即释锁，将源容器目录（或含空子目录的整棵目录树）经 `std::fs::copy` + `sync_all` 完整预拷至目标数据盘 **`<DATA_DIR>/<dst_disk>/.zxs/staging/<tx_id>/`**（`StagingGuard` 持有；若目标盘空间不足 `ENOSPC` 直接自动清理并返回 `507 Insufficient Storage`，源数据盘丝毫无损）；
    2. **短暂写锁双盘本盘切换**：持写锁复核源容器（或源目录子树）在预拷期间无任何并发修改/增删/移动（否则返 `409 Conflict` 并自动清暂存）且目标路径无冲突 → 若单容器 `tags`/时间戳变更则更新暂存内 `container.json` → 写并刷盘全局 `inflight/<tx_id>.json` → 目标盘补齐父目录并**本盘单次 `rename` 上线** → 源盘**本盘单次 `rename` 将原目录移入 `<DATA_DIR>/<src_disk>/.zxs/purging/<tx_id>/`** → 单事务更新 SQLite → 删 `inflight` → **立即释锁**，后台无锁清扫 `<src_disk>/.zxs/purging/<tx_id>/`。
  * **多回收盘按最大剩余空间选盘回收、还原与彻底清理**：
    * **选盘与移入回收站**（单容器 `DELETE /containers/{id}` 或整树 `DELETE /folders?recursive=true`）：若某块已登记 `<回收盘>` 与源数据盘 `st_dev` 相同（如本地单分区测试），写锁内直接同盘 `rename`；常规跨物理 NVMe 时，查询全部已登记 `<RECYCLE_DIR>/<回收盘>/` 的 `statvfs` 可用空间（整树批量回收时按容器体积逐个扣减估算余额，自动跨多块回收盘均衡落盘），为每个容器选定当前剩余空间最大的 `<回收盘>`，先无锁流拷 + `sync_all` 至 `<RECYCLE_DIR>/<回收盘>/.zxs/incoming/<tx_id>/<rid>/`（盘满返 `507` 且源数据盘未动分毫），再持短暂写锁复核源未变 → 写 `inflight` → 各回收盘本盘 `rename` 至 `<RECYCLE_DIR>/<回收盘>/<YYYY-MM-DD>_<rid>` → 源数据盘本盘 `rename` 摘入 `<src_disk>/.zxs/purging/<tx_id>/` → 单事务提交 SQLite（记录各自 `recycle_disk`）→ 删 `inflight` → 释锁后无锁清盘。
    * **整树删除的还原边界**：`DELETE /folders?recursive=true`（仅限盘内子目录，禁止删除一级数据盘目录）将子树内所有文件容器拆为各自独立的回收项；逐个还原时自动在目标数据盘重建缺失祖先目录，**不承诺还原原目录树内的纯空子目录**。
    * **跨盘还原与彻底清理**：还原（`restore`）从 `recycle_bin` 记录的 `<回收盘>` 无锁预拷 + `sync_all` 至目标 `<数据盘>/.zxs/staging/<tx_id>/`，再持写锁校验目标无冲突后在目标数据盘本盘 `rename` 入位，并将原回收目录本盘移入 `<回收盘>/.zxs/purging/<tx_id>/` 释锁清盘；单条彻底删除或清空全部回收站持写锁删 SQLite 记录并将各盘回收批次移入各自 `<回收盘>/.zxs/purging/<tx_id>/`，释锁后多盘并行清盘。
* **启动多盘挂载强校验、幂等前滚自愈与并行裸盘重建**：
  * **防漏挂载保护**：`zxs serve` 启动先校验 `config.toml` 中 `layout == 1` 并持全局 `.lock`，随后逐一核对 `config.toml` 已登记的全部 `data_disks` 与 `recycle_disks` 下 `<盘目录>/.zxs/disk.toml` 存在、可写且 `disk_id` 完全匹配——**任一物理数据盘或回收盘漏挂载、掉盘或挂错顺序均立即打印精确盘名并拒绝启动**，彻底杜绝写穿系统根分区或误把未挂载盘文件当成丢失（离线 `zxs verify` 仅校验 `data_disks` 就绪，不依赖回收盘挂载）；
  * **幂等前滚恢复**：因全局 `inflight/<tx_id>.json` 永远在目标盘暂存数据 `sync_all()` 落稳之后才写入，启动回放 `inflight/*.json` 只需幂等补完各盘剩余的本盘 `rename` 与 SQLite 更新（绝无半途拷贝或 `ENOSPC` 失败风险），随后并行清空所有数据盘与回收盘 `.zxs/{staging,incoming,purging}/` 残留并删 `inflight`；
  * **多盘并行 `rebuild-index`**（或 `index.sqlite` 缺失时自动触发）：按 `config.toml` 登记的 `data_disks` 并行扫描各 `<DATA_DIR>/<数据盘>/`（先将一级盘目录 `"/<数据盘>"` 写入 `folders`，**跳过盘根 `<数据盘>/.zxs/` 工作区**，再递归扫描盘内目录）：凡盘内子目录含 `.zxs/container.json`、或含 `.zxs/` 目录、或直接含 `v\d{6}/.zxs/version.json` 子目录，一律识别为文件容器并停止向下遍历——正常容器校验 `v{latest_version}` 原件完整且等于现存最高版本号；**发现 `container.json` 缺失损坏、无有效版本或 `v{latest_version}` 缺失损坏时，立即打印精确受损路径并报错拒启**；其余盘内目录写入 `folders` 表；并行扫描各 `<RECYCLE_DIR>/<回收盘>/` 一级目录（跳过盘根 `.zxs/`），识别含常规文件 `.zxs/recycle.json` 的回收批次写入 `recycle_bin`。

---

## 四、唯一通信契约：8 组极简 HTTP/1.1 接口

**全接口（含 `/payload` 媒体流播）统一且仅采用 `Authorization: Bearer <session_token>` 请求头鉴权**（桌面 CLI、桌面浏览器 UI 经 `127.0.0.1` Cookie 反代、iOS `AVURLAssetHTTPHeaderFieldsKey`、Android `VideoView.setVideoURI(uri, headers)` 四端全部原生支持，**彻底取消 URL `?token=` 传参**；密码永久存在客户端，换取永不过期的内存会话令牌，服务端重启返 `401` 时客户端自动静默重登，**仅当服务端设置新密码后才失效**）。流式上传通过 `Zxs-Params: <Base64URL(UTF-8 JSON)>`、`Content-Length: <u64>` 与可选头 `Zxs-Blake3: <64-hex>` 传递控制参数，请求体直接为 `application/octet-stream`（客户端大文件上传可省略 `Zxs-Blake3` 单遍读盘发流并在收到响应后核对服务端返回的 `blake3`；支持 `0` 字节空文件）。

| # | 方法与路径 | 核心语义 |
| :-: | :--- | :--- |
| **1** | `POST /sessions`<br>`DELETE /sessions/current` | 校验 Argon2id 密码签发**永不过期**的随机 256-bit 内存 Bearer Token（客户端永久保存密码并在服务端重启返 `401` 时自动静默重登，仅设置新密码后失效并清空本地密码提示重新登录）；主动登出清除本地保存的密码并销毁服务端内存会话（重试遇 `401` 视为已登出）。 |
| **2** | `POST /containers` | `Zxs-Params` 传 `{ folder, name, tags }`（`folder` 须位于已登记一级数据盘下，禁止 `"/"`），直传文件流至目标数据盘本盘 `.zxs/staging/` 并一步原子发布首版 `v1`（路径冲突返 `409`）。 |
| **3** | `POST /containers/{id}/versions` | `Zxs-Params` 传 `{ expected_version }`，直传新版文件流至所属数据盘 `.zxs/staging/`，本盘原子发布 `latest_version + 1`（版本不匹配或上传期间容器被移动/改名返 `409`）。 |
| **4** | `PATCH /containers/{id}` | JSON 请求体 `{ folder?, name?, tags? }`（目标 `folder` 禁止 `"/"`），修改标签、同数据盘瞬间原子移动/重命名（支持纯大小写改名），或跨物理数据盘自动执行“无锁预拷至目标盘 → 短暂写锁切换”（目标盘满返 `507`）。 |
| **5** | `GET /containers`<br>`GET /containers/{id}` | 跨全盘纯容器检索与稳定分页：按 `?folder=&tag=&q=&cursor=&limit=` 以 `(folder_key, name_key, id)` 字母升序返回 `{ containers, next_cursor }`（`folder=/` 返回空容器列表；不传 `folder` 时跨全部物理数据盘扁平搜索，列表项含 `latest_blake3` 但不含 `size`）；查看容器详情与现存升序 `versions` 清单（含每版 `size` 与 `blake3`）。 |
| **6** | `GET` & `HEAD` `/containers/{id}/versions/{v}/payload` | 仅认 `Authorization: Bearer` 头；读锁内打开对应数据盘文件句柄即释锁，通过 `spawn_blocking` + 同步 4 MiB 缓冲流式全量下载与标准单区间 HTTP `Range` 读取（供 4K/8K 视频秒开拖动及客户端 40 字节头部预判，附强 `ETag: "<blake3>"` 与 `nosniff`）。 |
| **7** | `DELETE /containers/{id}[?expected_version=<u32>]`<br>`GET /recycle`<br>`POST /recycle/{recycle_id}/restore`<br>`DELETE /recycle/{recycle_id}` & `DELETE /recycle` | 自动按最大剩余可用空间选定物理回收盘并平铺迁入 `<RECYCLE_DIR>/<回收盘>/<YYYY-MM-DD>_<rid>/`（支持可选 `expected_version` 前置版本校验，远端已更新返 `409`；回收盘满返 `507`）；跨全部回收盘按 `(recycled_at DESC, recycle_id DESC)` 统一分页查看回收站；指定 `{ folder?, name? }` 还原回对应数据盘（缺失盘内祖先自动重建，冲突返 `409`）；单条彻底删除或并行清空全部回收盘。 |
| **8** | `GET /folders?folder=<path>`<br>`POST /folders`<br>`PATCH /folders`<br>`DELETE /folders?folder=<path>[&recursive=true]` | 目录树 CRUD：`folder=/` 字母升序列出全部已挂载一级数据盘目录，盘内列出直接子目录（含空目录，不带 `count`）；在已登记数据盘内新建空子目录（同大小写已存在幂等返 `200`，补齐盘内缺失祖先）；同盘单次 `rename` 或跨数据盘“无锁预拷整树 + 短暂写锁切换”重命名/移动文件夹整树（`{ from, to }`，禁止作用于 `"/"` 或一级数据盘目录本身，禁止移入自身或子孙超 1024B）；删除盘内子目录（默认仅删空目录；`recursive=true` 将子树内全部容器按剩余空间迁入各回收盘并清除该子目录树，不承诺还原原空子目录）。 |

---

## 五、客户端、UI 设计规范与运维子命令

### 5.1 纯同步客户端、本机浏览器 UI 与 `.zxsignore` 整包工作区 (`zxs-client`)

* **双交互入口（合一单二进制 `zxs-cli`）**：
  * **完整 CLI 模式**：`login`、`logout`、`ls`、`mkdir`、`mv-folder`、`rmdir [--recursive]`、`detail`、`patch`、`checkout`、`status`、`sync`、`refresh`、`untrack`、`commit`、`init-bundle`、`recycle [ls | purge]`、`restore`、`ui`。
  * **本机图形模式（`zxs-cli ui`）**：绑定 `127.0.0.1:0` 随机端口，一次性 `#secret` 换取 `HttpOnly; SameSite=Strict` Cookie，强制校验 `Host` 与 `Origin`，托管内嵌零构建原生 ES2024 模块页面并自动唤起系统浏览器；浏览器视频播放经本机 `/media/` 反代并由 Rust 侧附带 `Authorization: Bearer`。
  * **永久密码登录与自动续接**：首次登录后客户端永久保存服务端地址与密码（桌面 `0600` 本地凭证文件、iOS Keychain、Android Keystore AES-256-GCM）；日常使用永不过期内存 Bearer Token，遇服务端重启返 `401` 自动静默重新登录并重试，**仅当服务端通过 `zxs reset-password` 设置新密码（或用户主动登出）时才清除本地密码并提示重新输入**。
* **本地工作区（`workspace.sqlite`）与破坏性操作前置核验**：
  * **默认镜像与内容核验保护**：`workspace.sqlite` 对 `container_id` 与 `local_path` 双设 `UNIQUE` 约束；`checkout` 未指定 `--to` 时默认落盘到 `<workspace>/<数据盘>/<子目录>/<name>`（目录包落 `<workspace>/<数据盘>/<子目录>/<去.zip名>`）；首次 `commit` 未指定 `--folder` 时默认取文件在工作区内的相对目录（若文件直接散落在工作区根下未归入任何 `<数据盘>/` 子目录，拒绝提交并提示放入对应盘目录或显式指定 `--folder /<数据盘>`）；本机执行 `patch`/`mv-folder` 或跨端移动改名后执行 `refresh` 时，同步迁移本地 `clean` 状态的镜像路径（macOS/Windows 纯大小写改名经临时中转名两步 `rename`）；**任何本地覆盖或自动搬移前，除 `(size, mtime_ns)` 外必须复核本地文件 BLAKE3 确认内容确与上次记录一致**，发现改动即停止并提示用户。
  * **历史版本恢复到工作区**。网页历史版本每行提供「恢复到工作区」，CLI 使用 `checkout <id> --version <N>`。客户端用同一原件接口读取所选版本。恢复旧版按本地待提交的修改处理。已有追踪项沿用原提交基线，首次恢复以当时读到的远端最新版为基线。同步保留本地待提交的内容，显式 `commit` 通过正常上传和版本前置校验发布新版。选择当前版按普通检出登记。
  * **同步预检和安全替换**。同步用容器 ID、版本号和 BLAKE3 判断是否需要更新。版本倒退或同号异哈希时保留本地并报冲突。最新版直接复用列表的版本号和哈希，从原件响应读取长度并校验正文。已知路径冲突和本地修改在下载正文前报告，本地已写盘的修改可在确认备份后恢复。整件校验完成后，将实际要替换的旧件同盘移入备份位置，再以无覆盖方式安装新件。目录包按成员变化处理，忽略项保留在原位。发布前按容器记录所选版本、提交基线和备份位置。成功后更新追踪并清除中断记录。中断可针对原版本重试，中断项暂停同步删除。
  * **全工作区扫描（`status`）**：以 `(size, mtime_ns)` 并行快速对比已追踪条目，同时扫描工作区内**未追踪的新增文件/新目录包**（跳过 `.zxs/`、符号链接与合法 `.zip` 工作区内部被 `.zxsignore` 忽略的条目；遇到符号链接跳过并提示；未追踪扫描设 **1,000 条熔断上限**，超限截断并标记 `truncated`），统一输出**已追踪状态（`clean` / `modified` / `missing` / `interrupted`）+ 未追踪新变更（`untracked`）+ 非法 `.zxsignore` 与符号链接警告**；Web UI 工作区直接列出未追踪文件并支持一键提交。
  * **丢包自动认领、冲突处置与带版本保护的同步删除**：上传末尾若断连丢回包，再次 `commit` 时服务端 `Expect: 100-continue` 立即返 `409`（零字节重传），客户端随即比对远端同容器/同路径的 `latest_blake3`——与本地文件哈希一致则自动认领成功并标为 `clean`；确属多端并发冲突则弹窗三选一（追加为 `latest_version + 1` / 另存新文件 / 备份本地后拉取远端最新版）；本地缺失的追踪条目支持“重新检出”或“同步删除”——**同步删除必向 `DELETE /containers/{id}` 传本地认知的 `expected_version`，远端已更新返 `409` 时强制提示重新确认**；跨端已删致远端 `404` 时绝不自动删本地文件，提示通过 `untrack` 保留本地文件转为未追踪新文件（或去回收站还原）。
* **`.zxsignore` / `.zip` 工作区整包收发与边界语义**：
  * **创建互斥、全写入口同名拦截与冲突兜底**：`.zip` 工作区只能在已登记数据盘内部（禁止将一级数据盘目录本身打包为 `.zip`）且**自身及所有子孙路径下均不存在任何已追踪文件容器**的目录下创建，`init-bundle` 检测到目录内已有容器时直接拒绝；客户端在**所有写入口（`init-bundle`、`commit`、`mkdir`、`mv-folder`、`patch`、`restore`）**统一执行同目录重名校验，禁止 `.zip` 工作区 `abc.zip` 与同目录普通子文件夹 `abc` 并存；若因离线 `zxs import` 等外部途径导致同目录已并存 `abc` 与 `abc.zip`，界面退回显示 `abc.zip`（配工作区图标 + 冲突提示）且默认 `checkout` 拒绝覆盖本地普通目录 `abc/`；若用户在已有文件容器的目录下手动创建 `.zxsignore`，`status` / `commit` / `checkout` 扫描时**必须忽略该 `.zxsignore` 并示警**，绝不静默接管或嵌套目录包；
  * **文件移入 / 移出 `.zip` 工作区的极简状态机语义（零监听、零猜谜）**：
    * **移入 `.zip` 工作区**：原路径下的独立追踪文件自然标为 `missing` 并明确提示 **「本地已删除或已移走：可确认删除原独立文件容器（带版本核对移入服务端回收站），或重新检出恢复」**；目标 `.zip` 工作区自然标为 `modified`（提交时将新文件并入 `.zip` 新版）；
    * **移出 `.zip` 工作区**：原 `.zip` 工作区标为 `modified`（提交新版移除该内部条目），移出到外部普通目录的文件自动识别为 `untracked` 并一律按 **「新的文件变更（新增未追踪文件，提交时创建全新独立文件容器）」** 传达与处理；
  * **确定性打包、40 字节列表预判与安全解包**：`init-bundle` 在目录根生成明文 `.zxsignore`，打包器仅按 `.zxsignore` 过滤（不跟随符号链接，保留未被忽略的空子目录 `dir/` 条目），**强制把 `.zxsignore` 排为 ZIP 第 0 个条目（偏移 0 字节处）**，其余条目按 NFC 归一化 UTF-8 路径字节序排列、固定归零时间；`≤ 4 MiB` 条目按有界内存窗口（`40–64 MiB`）由 `rayon` 多核并行 `zlib-rs` Level-1 压缩合并，`> 4 MiB` 条目单路流式直写（超限自动 ZIP64）；客户端列表展示遇到远端 `.zip` 容器时，先查本地追踪或进程内内存缓存（`LazyLock<Mutex<...>>`，按服务端地址、容器 ID、版本号及 BLAKE3 标识，上限 4,096 条），未命中则发 **`Range: bytes=0-39`（40 字节）** 核对 `PK\x03\x04` + 文件名长 10 + `".zxsignore"` 并缓存（`0` 字节返 `416` 或不足 40 字节按普通 `.zip` 缓存 `false`，并发上限 4）；真正 **`checkout` / `refresh` 解包时校验完整 ZIP 归档结构与常规文件属性**，实施**防 Zip Bomb 保护**（单包条目上限 `200,000`、解压总字节上限默认 `64 GiB`、解包前校验目标盘剩余空间、拦截 `..` 与符号链接穿越）；解包前在 `workspace.sqlite` 标记 `interrupted = true`，逐文件同目录临时文件原子 `rename` 写入，全部完成后清除 `interrupted`（中途断电则在下次 `status` 提示检出中断，**绝不触碰本地被 `.zxsignore` 忽略的文件**）。
* **移动端薄壳（`crates/ffi` + iOS 17+ / Android 13+）**：
  * `zxs_call` / `zxs_free` 提供极简 JSON 请求分发：`browse` 组合 `GET /folders` + `GET /containers` 并直接附带 `.zip` 工作区显示标记（Swift/Java 零 ZIP 解析代码）；文件打开收口为单往返 `open`（音视频直返 `/payload` URL 与当前 `Authorization: Bearer` 头供 iOS `AVPlayer` / Android `VideoView` 原生请求头流播，文档走 `{id}-{version}-{blake3}` 内容寻址有界 LRU 缓存，上限 64 份 / 512 MiB）；
  * **多盘相册备份与防重名覆盖（幂等递增）**：备份页支持选择目标数据盘（单数据盘自动默认 `/<数据盘>/相册`，多数据盘支持切换并本地记忆所选 `<数据盘>`）；向 `/<数据盘>/相册` 备份遇到同名文件时，直接利用 `GET /containers?folder=/<数据盘>/相册` 返回的 `latest_blake3` 依次检查 `name`、`name (2)`、`name (3)`…：命中一致则幂等跳过，均不匹配则以首个空闲递增名创建独立容器，绝不合并不同照片；
  * 移动端点击识别为 `.zip` 工作区的文件夹时，明确提示“目录工作区请在桌面端检出编辑，或选择导出整包”；iOS 使用 Keychain、Android 使用 Keystore AES-256-GCM 永久保存密码与服务端地址并接入 `OnBackInvokedCallback` 返回导航。

### 5.2 跨端 UI 审美与实现硬约束（静谧精密风格）

* **色彩与双主题**：源码禁止裸写 hex，统一采用语义 Token（`--bg-canvas/surface/elevated/subtle`、`--border-default/subtle`、`--text-primary/secondary/muted`、克制钴蓝 `--accent`、语义状态色 `--ok/warn/danger`），原生自适应暗黑模式。
* **排版与网格**：系统无衬线 + 等宽字体栈；严格 **6 级字阶 × 3 字重（`400 / 500 / 600`）**；严格 **4px / 8px 间距网格**与同心圆角（`12 / 8 / 6 / 5px`）；所有版本、大小（`KiB/MiB/GiB`）、时间（`YYYY-MM-DD HH:mm`）启用 `tabular-nums`。
* **图标与交互**：严禁用 Unicode 杂项符充当图标（桌面统一 `16×16` `1.5px` 描边内联 SVG，iOS 用 SF Symbols，Android 用 VectorDrawable）；严禁 `window.confirm` / `window.alert`；每视觉域最多 1 个主按钮；列表严格零缩略图且只展示 `GET /containers` 字段（`size` 仅在详情与版本历史显示）；**目录浏览与搜索检索解耦**——根 `"/"` 视图直观展示已挂载的各一级数据盘目录（根下禁用直接上传/新建目录，一级盘目录配磁盘图标且隐藏重命名/删除入口），进入任意数据盘后支持完整子目录新建/重命名/跨盘或同盘移动/删除（非空删除弹窗告知移入回收站）与文件操作；回收站支持还原、单条彻底删除与清空回收站（二次确认）；关键词或标签筛选直接调 `GET /containers?q=...&tag=...` 跨全部数据盘扁平展示匹配文件（附完整盘与目录路径）；**识别为 `.zxsignore` 工作区的 `.zip` 容器在全端一律显示为去 `.zip` 名的文件夹（配“文件夹+工作区徽标”专属图标）**；异常状态（非法 `.zxsignore`、符号链接、`missing` 待删原容器、远端 `404` 追踪项）显式警示并提供一键处置按钮；桌面端支持 `/`, `↑/↓`, `Enter`, `Backspace`, `Esc` 键盘流与宽屏推挤式 Inspector。

### 5.3 服务端离线运维子命令 (`zxs-server`)

所有离线运维子命令启动时均须校验 `layout == 1`、获取 `<DATA_DIR>/.zxs/.lock` 排他锁（在线服务运行中则报错拒绝并发改盘），并核对所需已登记物理盘 `.zxs/disk.toml` 挂载就绪：

* `zxs serve [--data-dir <path>] [--recycle-dir <path>] [--listen <addr:port>]`：启动在线服务（默认监听 `0.0.0.0`，强校验全部已登记 `data_disks` 与 `recycle_disks` 挂载就绪）；
* `zxs init --data-dir <path> --recycle-dir <path>` / `zxs sync-disks` / `zxs reset-password`：`init` 扫描两根目录下现有全部一级物理盘挂载子目录（要求数据盘与回收盘各至少 1 个），在每块盘内写入 `.zxs/disk.toml` 并在 `<DATA_DIR>/.zxs/config.toml` 登记盘清单与 `layout = 1`；后续新增挂载物理盘时运行离线 `zxs sync-disks` 自动初始化并增量登记新盘（不触碰已有盘数据）；`reset-password` 更新 Argon2id 密码哈希使旧客户端保存的旧密码失效；
* `zxs import <src_dir> --folder <target_folder>`：本机 NVMe 多线程单遍读盘哈希高速导入普通文件至指定数据盘目录（`--folder` 须位于已登记数据盘下，保留源目录结构与空目录，跳过符号链接，执行 NFC 校验并将原件设为 `0444` 只读）；
* `zxs prune --keep-last <N> [--container <cid>]`：离线清理旧历史版本释放空间（强制 **`N >= 1`**，至少保留最新 1 版，严禁删成空壳容器）；
* `zxs verify` / `zxs rebuild-index`：多盘并行 BLAKE3 全盘巡检（仅需 `data_disks` 就绪，报告受损原件、无 `container.json` 残留容器或缺失最新版本号并拒绝带病启服）、跨全部已挂载数据盘与回收盘并行扫描重建全局 `index.sqlite`。

---

## 六、代码设计规范与系统化工程准则

1. **项目内四层单向依赖**：严格按 **`接入外壳层 (api/cli/ui) → 用例编排层 (service) → 基础设施层 (store/transport) → 纯领域层 (domain)`** 自上而下调用。物理路径拼装只准写在 `store/layout.rs`，SQL 语句只准写在 `store/db.rs`，`domain` 层零 I/O。
2. **模块与规模硬红线**：Rust 统一使用 `foo.rs` + `foo/bar.rs`（**禁止新建 `mod.rs`**）；桌面 Web 前端按原生 ES Modules 拆分为多个独立小模块（零打包器）；**所有生产源码单文件 ≤ 350 行**（不含测试），**单函数 ≤ 60 行**，最大缩进 **≤ 3 层**，**函数参数 ≤ 4 个**（超限封装为结构体）。
3. **强类型与枚举状态机**：外部输入在边界第一时间解析为 `ContainerId`、`DiskName`、`FolderPath`、`FileName`、`VersionNum`、`Blake3Hash`；状态机一律使用互斥 `enum`；所有 JSON/TOML 输入结构体强制 `#[serde(deny_unknown_fields)]`。
4. **静态门禁与错误透明**：根 `Cargo.toml` 固化 `unsafe_code = "forbid"`（仅 `crates/ffi` 允许隔离的 FFI `unsafe`），并 `deny` 全部 `unwrap_used`、`expect_used`、`panic`、`todo`、`dbg_macro`；CI 开启 `-D clippy::pedantic -D warnings`；用 `thiserror` 保留完整上下文链。
5. **RAII 守卫、单锁序与有界内存**：所有本盘临时目录必须由 `StagingGuard` / `TempDir` 持有；全局锁序简化为单层 **`全局 RwLock（下载 open 与跨盘移动/回收拍快照拿读锁即释锁；写发布/同盘移动/跨盘切换拿写锁）→ 各盘本盘写入/rename → SQLite 写连接`**（网络收流、4 MiB 落盘、BLAKE3 哈希与跨盘流拷清盘全在无锁区完成；持数据库锁期间严禁文件流与网络 I/O）；**严禁对用户正文调用 `fs::read`/`read_to_end` 整文件入内存**（固定 `4 MiB` 缓冲流），元数据小文件读取强制设 `1 MiB` 上限。
6. **统一词表与跨项目冒烟门禁**：全栈统一使用 `Container`、`Version`、`Folder`、`LocalPath`、`Recycle` 五个核心名词；两个项目各自补齐核心状态机单元测试，并由 `tests/smoke.py`（配置多数据盘 + 多回收盘目录）真连对打拦截协议漂移。

---

## 七、性能目标与实施里程碑

* **可测端到端性能与内存目标（NVMe + 对应物理链路实测）**：单流读写跑满对应物理网卡与目标物理 NVMe 链路上限（10GbE 接近 **1.1+ GB/s**，25GbE / 40GbE / 雷电直连达 **2.5 – 4+ GB/s**，多客户端访问不同物理 NVMe 磁盘时吞吐线性叠加）；1 GB 代码库多核打包上传 **< 1.5 秒**；50 万容器冷启动 **< 50 ms**、多盘并行重建索引 **< 3 秒**；进程常驻缓冲内存 **< 128 MiB**。

| 阶段 | 子项目 | 交付内容与准出硬门槛 |
| :--- | :---: | :--- |
| **S1：Linux 多盘服务端定版** | `zxs-server` | 完成 `vendor/` 剪裁脚本、`layout = 1` 与多盘 `.zxs/disk.toml` 防漏挂载强校验、`init`/`sync-disks` 多盘登记、一级盘边界与 NFC 跨平台路径校验、只读（`0444`）真实目录树与真实文件名容器内核（目标数据盘本盘 `.zxs/staging` 流式发布、同盘瞬间移动与跨数据盘“无锁预拷+写锁切换”容器/整树移动、多回收盘按 `statvfs` 最大剩余空间自动选盘回收与跨盘还原、受损容器快速报错）、5 表全局 SQLite 索引、8 组 HTTP 接口（全接口统一 Bearer 头、永不过期内存会话、`DELETE` 版本前置校验）与全部排他锁运维子命令；`--offline` 零警告，漏挂载拒启、断电前滚自愈、跨盘移动/回收/还原 `507`/`409` 与线速吞吐测试全绿。 |
| **C1：客户端核心与 CLI** | `zxs-client` | 完成独立 `vendor/`、纯同步 `crates/core` 与完整 `apps/cli`（含 `mkdir`/`mv-folder`/`rmdir --recursive`/`untrack`/`recycle purge`）；联调多盘 `zxs-server` 跑通永久密码存储与重启静默重登、`<workspace>/<数据盘>/...` 默认镜像路径、覆写前 BLAKE3 核验、`interrupted` 检出中断提示、带 `expected_version` 同步删除、单遍大文件上传与丢包 `latest_blake3` 自动认领、全工作区扫描（含熔断、非法 `.zxsignore` 与符号链接示警）、全入口同名工作区拦截、`.zxsignore` 首条目打包、40 字节列表预判与防 Zip Bomb 限制。 |
| **C2：本机浏览器图形界面** | `zxs-client` | 在 `apps/cli` 完成 `ui` 子命令（`127.0.0.1` 回环服务与 `/media/` 反代 + 内嵌零 npm 原生 ES2024 模块页面，满足 §5.2 全部审美、暗黑模式、根下物理数据盘视图、工作区专属图标、同盘/跨盘目录管理与回收站清理规范），三大桌面系统浏览器实测文件管理、工作区一键提交、唤起本地编辑与 4K/8K 原片秒拖播放。 |
| **C3：移动端原生接入** | `zxs-client` | 完成 `crates/ffi`（约 200 行零依赖 C-ABI + 内容寻址有界文档缓存 + 多数据盘选择与备份重名幂等递增重命名 + Bearer 请求头原生流播）与 iOS 17+ / Android 13+ 薄壳（Keychain / Keystore 永久保存密码 + 系统返回导航 + `.zip` 工作区专属文件夹展示），完成局域网相册高速备份与流播实机验收。 |
