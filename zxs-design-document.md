# ZXS 总体架构与系统设计规格书

> **文档状态**：与当前仓库源码（2026-11-27 修订版）保持一致。
>
> **系统定位**：面向个人超高速局域网（10GbE / 25GbE / 40GbE / 雷电直连）、全 NVMe 固态存储与纯现代 64 位软硬件栈的文件与版本管理系统，用于管理办公文档、照片、视频、代码/文本项目与普通文件。全系统由 **`zxs-server`（Linux 服务端）**、**`zxs-client`（跨端客户端）** 与 **`tools/zxs-prepare`（独立磁盘准备工具）** 组成；两端依赖隔离，协议变更时同步升级。

### 章节导览

| 章节 | 主题 | 适合读者 |
| :---: | :--- | :--- |
| [一](#一目标基线设计原则与红线) | 软硬件基线、发布形态、核心原则与反过度设计红线 | 所有读者 |
| [二](#二仓库拓扑与依赖治理) | 仓库目录结构、直接依赖选型与 `vendor/` 离线剪裁规范 | 开发 / 构建 |
| [三](#三服务端多盘存储架构-zxs-server) | 多物理盘布局、元数据格式、SQLite 索引、原子写与启动恢复 | 服务端开发 / 运维 |
| [四](#四http11-通信契约8-组接口) | 鉴权方式、流式上传头约定与 8 组 HTTP 接口契约 | 全栈开发 |
| [五](#五跨平台客户端与交互规范-zxs-client) | 桌面 CLI / Web UI、本地工作区、`.zip` 目录包、移动端与 UI 约束 | 客户端 / 移动端开发 |
| [六](#六离线运维与独立磁盘准备工具) | 服务端离线子命令与 `tools/zxs-prepare` 磁盘准备流程 | 运维 / 部署 |
| [七](#七工程规范与静态门禁) | 四层架构、代码行数预算、静态门禁、锁序与测试要求 | 所有开发 |
| [八](#八性能目标与交付里程碑) | 可测端到端性能目标与 S1–C3 交付准出标准 | 项目验收 |

---

## 一、目标基线、设计原则与红线

### 1.1 现代软硬件基线

| 维度 | 支持基线 | 工程收益 |
| :--- | :--- | :--- |
| **CPU 指令集** | **x86_64**：`x86-64-v3`（AVX2 + BMI2 + FMA + PCLMULQDQ，自建可开 `target-cpu=native`）<br>**ARM64**：Apple Silicon（M1+）/ ARMv8.2-A+（NEON + 硬件 CRC32） | 默认全开 SIMD 向量化，BLAKE3 与 ZIP 压缩无标量慢速回退；不构建 32 位或旧架构产物。 |
| **服务端 OS** | **仅现代 Linux 6.6+ LTS**（`x86_64` / `aarch64`） | POSIX 语义一致（流读期间可直接原子重命名/移动目录）；原生支持 `--data-dir` 与 `--recycle-dir` 按一级子目录挂载多块独立物理 NVMe。 |
| **桌面客户端 OS** | **Linux 6.6+** · **macOS 15+**（Apple Silicon） · **Windows 11 24H2+**（`x86_64-msvc`） | 单一纯同步客户端二进制（CLI + `127.0.0.1` 本机浏览器 UI），零重型 GUI 框架。 |
| **移动客户端 OS** | **iOS 17+**（`aarch64-apple-ios`） · **Android 13+**（`aarch64-linux-android`） | 通过零依赖 C-ABI（`crates/ffi`）接入轻量原生壳，支持文件/版本提交、相册上传、媒体流播与文档查看。 |
| **语言基线** | **Rust Edition 2024**（稳定版 `1.100.0`） | 本地与 CI 锁定统一工具链；标准库文件锁覆盖 Android，`DropGuard` 管理客户端发布与缓存临时文件。 |

**发布标签与产物划分**

| 标签前缀 | 包含产物 | 补充约定 |
| :--- | :--- | :--- |
| `v*`（联合发布） | Linux x86_64 服务端包（内含磁盘准备工具）+ 全套 5 个客户端包 | 协议或布局变更时按此同步升级。 |
| `client-v*` | Linux x86_64、Windows x64、macOS ARM64、Android ARM64、iOS ARM64 | Android 正式包沿用同一发行密钥签名。 |
| `prepare-v*` | Linux x86_64、Windows x64、macOS ARM64 磁盘准备工具 | 独立维护与发布。 |

### 1.2 五条核心原则

1. **双项目独立与同步升级**
   `zxs-server` 与 `zxs-client` 各自维护源码和 `vendor/`。HTTP 接口采用第四章固定路径，不设协议版本号；协议变更时两端一并升级。升级服务端前先由旧版完成在途操作并停服，按新结构准备存量数据后再启动新版。
2. **双根目录多盘直挂与只读裸盘直读**
   服务端管理一个 `--data-dir` 与一个 `--recycle-dir`，各自的一级子目录分别挂载 `N` 块数据物理盘与 `M` 块回收物理盘（盘根 `.zxs/disk.toml` 校验盘身份）。数据盘内直接保存真实目录树（含空目录）和以真实文件名命名的容器目录（禁止 UUID/哈希目录名），各版本以 `v000001` 递增存放 `0444` 只读原件。拔下任意单盘即可只读直取完整目录树；元数据不冗余存路径，同盘移动目录只需一次 `rename`；全盘统一由可随时重建的 `index.sqlite` 加速检索。
3. **本盘就近暂存 + 短暂写锁切换**
   上传直接在目标数据盘的 `.zxs/staging/` 内落盘。跨盘移动、回收与还原统一先在目标盘无锁复制并校验原件，再在短暂写锁内完成各盘本盘原子 `rename`。跨盘目录迁移与整树回收期间仅冻结相交写入，等待与复制均在全局发布锁外完成；失败或断连由暂存守卫自动清理未发布副本。
4. **单调线性版本与回收盘自动选盘**
   版本号单调递增，`prune` 仅清理保留范围前缀且必留最新版。删除容器时按 `statvfs` 选定可用空间最大的在线回收盘，平铺迁入 `<RECYCLE_DIR>/<回收盘>/<YYYY-MM-DD>_<recycle_id>/`（保留真实文件名，还原时由用户指定在线目标目录）。
5. **服务端纯粹单文件 + 客户端 `.zxsignore` 整包收发**
   服务端不感知也不解析 `.zip`。客户端将含 `.zxsignore` 的本地目录多核打包为标准 `.zip` 整包收发；移动应用打开目录包后在应用内浏览成员。

### 1.3 反过度设计红线

* **不做逻辑卷池化与掉盘降级状态机**：启动时按 `.zxs/disk.toml` 识别在线盘；在线数据盘直接呈现为根 `"/"` 下的一级目录，回收盘仅做 `statvfs` 最大剩余空间选盘；挂载变更在停服时完成，离线盘接回后重启识别。
* **不做分块断点续传状态机与 `multipart`**：不建 `uploads/` 续传池、偏移锁与过期回收线程。
* **不引入重型 GUI 与移动第三方框架**：桌面 UI 由 `zxs-cli ui` 托管零构建原生 ES2024 页面；iOS 用纯 SwiftUI，Android 用纯 SDK 原生 View（零 AndroidX / Material）。
* **不做远端 `.zip` 局部拆包与服务端缩略图**：目录包上传、检出和移动端打开一律整包传输；HTTP `Range` 仅用于音视频原件播放；桌面检出在完整校验 ZIP 原件后本地识别类型；列表零缩略图。
* **客户端核心不引入异步运行时**：统一采用纯同步 I/O + `ureq` 连接池 + `rayon` 线程池。
* **不做协议版本协商、分支版本图、后台自动同步守护进程、内置文档编辑器及未到阶段的空壳模块**。

---

## 二、仓库拓扑与依赖治理

### 2.1 目录结构

```text
/
├── README.md                      # 总索引、快速上手与 HTTP 协议契约说明
├── zxs-design-document.md         # 本规格书
├── tools/zxs-prepare/             # 跨平台独立磁盘准备工具（CLI + 本机浏览器界面）
├── tests/
│   ├── run.py                     # 分层运行 Rust、集成、桌面、浏览器与 Android 测试
│   └── smoke.py                   # 服务端、CLI 与 C ABI 的端到端冒烟测试
│
├── zxs-server/                    # 【子项目 1：Linux 存储服务端单二进制】
│   ├── .cargo/config.toml         # 锁定 ./vendor 离线源与 Linux 现代 CPU 参数
│   ├── Cargo.toml                 # 单包工程（forbid(unsafe_code)）
│   ├── Cargo.lock / VENDOR.toml / VENDOR.sha256
│   ├── tools/sync_vendor.py       # 独立 vendor 同步与剪裁脚本
│   ├── vendor/                    # 服务端离线依赖（零压缩库、零 GUI 库）
│   └── src/
│       ├── main.rs                # 入口：serve | init | sync-disks | import | prune | verify | rebuild-index | reset-password
│       ├── domain/                # 强类型 ID、一级盘边界、NFC 路径与保留名校验、错误码（零 I/O）
│       ├── store/                 # 路径布局、文件系统操作、事务恢复与 SQLite 索引
│       ├── service/               # 上传发布、同盘/跨盘移动、多盘回收还原、认证与离线运维
│       └── api/                   # axum HTTP/1.1 路由、鉴权与 Range 原件流
│
└── zxs-client/                    # 【子项目 2：跨平台客户端套件】
    ├── .cargo/config.toml         # 锁定 ./vendor 离线源与全端现代 CPU 参数
    ├── Cargo.toml                 # 纯同步 Workspace（core + cli + ffi）
    ├── Cargo.lock / VENDOR.toml / VENDOR.sha256
    ├── tools/sync_vendor.py       # 独立 vendor 同步与剪裁脚本
    ├── vendor/                    # 客户端离线依赖（零异步运行时、零重型 GUI 包）
    ├── crates/
    │   ├── core/                  # 纯同步内核：domain / store(workspace.sqlite) / transport / zip_bundle / service
    │   └── ffi/                   # 移动端 C ABI、传输进度回调与有界文档缓存
    └── apps/
        ├── cli/                   # 桌面单二进制：完整 CLI + `zxs-cli ui`（内嵌原生 ES2024 页面）
        ├── ios/                   # iOS 17+ SwiftUI 薄壳（Keychain + PhotosPicker + AVPlayer + QuickLook）
        └── android/               # Android 13+ 纯 SDK 薄壳（Keystore AES-GCM + PhotoPicker + VideoView + ContentProvider）
```

### 2.2 直接依赖选型与 `vendor/` 剪裁规则

| 子项目 | 核心直接依赖 | 关键限制 |
| :--- | :--- | :--- |
| **`zxs-server`** | `tokio`（`rt-multi-thread, macros, net, io-util, time, signal, sync`）、`axum`（`http1, json, query, tokio`）、`futures-core`、`rustix`（`fs`，`statvfs` 选盘）、`rusqlite`（`bundled`）、`blake3`（`rayon`）、`rayon`、`argon2`、`subtle`、`zeroize`、`base64`、`unicode-normalization`、`getrandom`、`serde`、`serde_json`、`toml`、`thiserror`、`tracing`、`tracing-subscriber`（`fmt, ansi`）、`clap`（`derive, env`） | 禁止 `tokio/fs`、`tokio-util`、`futures-util`；所有磁盘流读写统一走 `spawn_blocking` + 同步 `std::fs::File`。 |
| **`zxs-client`** | `ureq`（关闭默认 TLS）、`tiny_http`（仅 `zxs-cli ui` 本机回环）、`rustix`（Unix `fs`）+ `winsafe`（Windows `kernel`）、`rusqlite`（`bundled`）、`blake3`（`rayon`）、`rayon`、`zip`（关闭默认特性，启用 `deflate-flate2-zlib-rs`）、`ignore`、`base64`、`unicode-normalization`、`getrandom`、`serde`、`serde_json`、`thiserror`、`clap`（`derive, env`） | 零异步运行时、零 TLS 依赖；客户端对 `tiny_http 0.12.0` 保留一行连接调度修正（计算空闲线程时扣除已排队连接），由 `sync_vendor.py` 自动复写并计入客户端行数。 |

**`sync_vendor.py` 统一剪裁规范**

* 删除各包内的 `tests/`、`benches/`、`examples/`、`docs/`、`sqlcipher/`、`test-macros/`、`test-data/`，以及所有 32 位、Intel Mac、`gnu`/`gnullvm`、BSD/WASI 非目标平台文件。
* 重写 `.cargo-checksum.json` 为 `{"package": "<upstream_sha256>", "files": {}}` 并生成 `VENDOR.sha256`；构建与测试全程强制 `--offline --locked`。

---

## 三、服务端多盘存储架构 (`zxs-server`)

### 3.1 标识符、命名空间与网络边界

#### 1. 强类型标识符

| 标识符 | 格式 | 职责与限制 |
| :--- | :--- | :--- |
| `container_id` | 32 位小写十六进制（UUIDv7，128-bit） | API 路由、SQLite 主键、客户端追踪与在途日志标识；**严禁用作磁盘容器目录名**。 |
| `recycle_id` / `disk_id` / `tx_id` | 32 位小写十六进制（128-bit 随机） | 回收批次、物理盘身份与内部事务标识。 |
| `session_token` | 64 位小写十六进制（256-bit 随机） | 纯内存 Bearer 鉴权令牌。 |
| `version` | 单调递增 `u32` | 磁盘版本目录固定 6 位补零（`v000001`）。 |

#### 2. 跨平台路径与命名空间规则

* **Unicode NFC 与合法字符集**：入口对所有目录段名（含盘名）、`name` 及 `.zip` 内部相对路径做 **Unicode NFC 归一化**。`folder` 以 `/` 开头、不以 `/` 结尾（根为 `"/"`）；单段名与 `name` 为 `1..=255` UTF-8 字节，完整逻辑路径（`folder` + `name`）总长 `≤ 1024` UTF-8 字节。
* **禁止字符与保留名**：拒绝 `/`、`\`、`\0`、控制字符、`< > : " | ? *`、段尾空格或句点、`.`、`..`，以及 Windows 设备名（`CON`、`PRN`、`AUX`、`NUL`、`COM1..9`、`LPT1..9`，不区分大小写且含带扩展名形式如 `aux.txt`）。
* **全局唯一保留名 `.zxs`**（不区分大小写）：禁止用作任何目录段名、盘名或文件名，彻底隔离系统元数据与用户文件（用户可自由存储 `container.json`、`version.json`、`recycle.json`、`disk.toml`）。
* **一级数据盘边界**：根 `"/"` 下的一级目录 `"/<数据盘名>"` 固定映射 `<DATA_DIR>/<数据盘名>/`。禁止直接在 `"/"` 下创建文件容器（`folder == "/"` 返回 `400`），禁止通过在线 API 新建、移动、重命名或删除一级数据盘目录本身（返回 `400/409`）。
* **统一大小写不敏感树**：普通目录与文件容器共用同一棵大小写不敏感路径树（保留首次创建或显式改名拼写）。同目录下禁止重名（自身纯大小写改名除外）、禁止路径既作目录又作容器、禁止并存仅大小写不同的目录（冲突返 `409` 并提示已有拼写）。
* **只读真实文件名原件**：版本目录内直接以提交时文件名 `name_at_commit` 存放原件，发布时设 `0444` 只读权限。容器改名只重命名容器目录，历史版本原件名不可变；裸盘直读仅供只读浏览与应急取回，严禁原地修改。

#### 3. 网络监听与传输安全

* 服务端默认监听 `0.0.0.0:7738`（可通过 `--listen` 或 `config.toml` 配置），不做网段拦截。
* 全端客户端零 TLS 依赖，直连 `http://`；跨不可信网络或公网访问由透明网络层加密隧道（WireGuard / Tailscale 等）提供传输保护。

### 3.2 多物理盘布局与元文件格式

初始化要求 `--data-dir` 与 `--recycle-dir` 各至少包含一个由 `zxs-prepare` 准备就绪的一级子目录（部署时每目录挂载独立物理 NVMe；测试可用普通目录 `disk1/`、`rec1/`，回收与还原仍走先复制后切换）。单块数据盘可独立只读直取原件；启动与 `zxs verify` 均按 `.zxs/disk.toml` 识别在线盘并报告缺席盘。

```text
<DATA_DIR>/                                  # 全局系统目录与数据盘挂载根
├── .zxs/                                    # 全局系统元数据
│   ├── config.toml                          # 密码哈希、监听地址、recycle_dir、已登记 data_disks 与 recycle_disks
│   ├── .lock                                # File::try_lock() 全局排他锁（在线服务与离线命令互斥）
│   ├── inflight/
│   │   ├── <tx_id>.json                     # 全局原子事务在途意图日志
│   │   └── <tx_id>.items/plan.json          # 递归回收的整树计划
│   └── index.sqlite                         # 启动时从在线盘重建的统一 SQLite WAL 索引
├── nvme1/                                   # 【物理数据盘 1】（逻辑根下显示为 /nvme1）
│   ├── .zxs/                                # 本盘专属工作区（保障上传与切盘为本盘原子 rename）
│   │   ├── disk.toml                        # kind = "data", disk_id
│   │   ├── staging/<tx_id>/                 # 本盘上传、跨盘迁入与还原暂存区
│   │   └── purging/<tx_id>/                 # 移出可见目录树的源目录待清区
│   └── 文档/
│       └── 2026/
│           └── 方案.docx/                   # 容器目录名 = 真实文件名
│               ├── .zxs                     # 容器元数据 JSON 文件
│               ├── v000001/
│               │   ├── .zxs                 # 版本元数据 JSON 文件（发布后不可变）
│               │   └── 方案.docx            # 0444 只读真实文件名原件
│               └── v000002/...
└── nvme2/                                   # 【物理数据盘 2】（逻辑路径 /nvme2/...）
    └── ...

<RECYCLE_DIR>/                               # 回收盘挂载根
├── rec1/                                    # 【物理回收盘 1】
│   ├── .zxs/
│   │   ├── disk.toml                        # kind = "recycle", disk_id
│   │   ├── incoming/<tx_id>/<recycle_id>/   # 无锁预拷贝暂存区
│   │   └── purging/<tx_id>/                 # 已删除或已还原批次的待清区
│   └── <YYYY-MM-DD>_<recycle_id>/           # 平铺单层回收批次目录
│       ├── .zxs                             # 回收批次元数据 JSON 文件
│       └── 方案.docx/...                    # 完整移入的原容器目录
└── rec2/                                    # 【物理回收盘 2】（按剩余空间自动分担）
    └── ...
```

**三类 `.zxs` 元数据文件与目录规则**

| 元文件位置 | JSON 字段 | 核心语义 |
| :--- | :--- | :--- |
| `<容器>/.zxs` | `container_id`, `tags`, `first_retained_version`, `latest_version`, `created_at`, `updated_at` | 不存 `folder`/`name`（路径由磁盘相对位置唯一决定）。保留范围 `[first_retained_version, latest_version]` 闭区间内每版目录与原件均须存在；`prune` 在同一在途事务中删前缀、刷盘容器目录后推进首保留版。`tags` 为容器级逻辑标签，筛选时保留已存标签的首尾空格；改标签不升版本、不中断上传。 |
| `vNNNNNN/.zxs` | `version`, `created_at`, `name_at_commit`, `size`, `blake3` | 与原件同级并随版本目录原子发布，发布后永不修改；原件名恒等于 `name_at_commit`。 |
| `<回收批次>/.zxs` | `recycle_id`, `recycled_at`, `original_name` | 与回收容器同级，省略源目录；还原时由用户指定在线目标目录。 |

* **空目录保留规则**：空文件夹是一等目录实体，落盘于真实路径并记入 `folders` 表；容器移走、改名或回收后变空的父目录保留在原位，仅通过显式 `DELETE /folders` 删除。

### 3.3 全局索引 (`index.sqlite`) 与会话认证

* **连接池配置**：`WAL + synchronous=FULL + busy_timeout=5s`，1 写连接 + 8 非阻塞轮询读连接。
* **五张 `STRICT` 业务表**（无会话表、无回执表、无隔离表）：

| 表名 | 关键列与索引 | 职责 |
| :--- | :--- | :--- |
| `folders` | `path_key`（PK，NFC+小写）、`parent_key`（建索引）、`path`（显示拼写） | 存全量目录树（含一级数据盘与空目录），支撑子目录字母序列表。 |
| `containers` | `id`（PK）、`folder`/`folder_key`、`name`/`name_key`、`path_key`（UNIQUE）、`latest_blake3`、`data`；索引 `(folder_key, name_key, id)` | 支撑单目录与跨全盘自然字母序稳定分页、丢包认领与相册去重。 |
| `versions` | 容器版本明细（`size`、`blake3` 等） | 支撑详情查询、原件打开校验与回收空间估算。 |
| `tags` | 容器逻辑标签映射 | 支撑全盘按标签筛选。 |
| `recycle_bin` | 回收记录 + 所属 `recycle_disk` | 支撑跨在线回收盘统一倒序分页，还原与清理免扫描直达目标盘。 |

* **会话与永久密码机制**：服务端仅在内存中维护已签发 `session_token` 的 BLAKE3 哈希集合（**永不过期**）。客户端永久安全保存服务器地址与密码（桌面 `0600` 文件、iOS Keychain、Android Keystore AES-256-GCM）；服务端重启返 `401` 时客户端静默重登并重试，**仅当服务端执行 `zxs reset-password` 或用户主动登出时才失效**。

### 3.4 原子写路径与跨盘事务

#### 1. 流式上传与本盘原子发布

1. **收流前预检（`Expect: 100-continue`）**：校验目标路径位于已登记在线数据盘、无命名空间冲突且匹配 `expected_version`，冲突在读正文前返 `409`。
2. **本盘无锁落盘**：在 `<DATA_DIR>/<数据盘>/.zxs/staging/<tx_id>/version/`（由 `StagingGuard` 持有）按 **4 MiB** 块流式写入 `<真实文件名>` 并行计算 `blake3::update_rayon`；支持 `0` 字节空文件（跳过长传输名额）；写盘提前失败即停止读正文并清暂存。流结束核对 `Content-Length` 与可选 `Zxs-Blake3`，设原件 `0444`，写版本 `.zxs` 并刷盘文件与目录。
3. **写锁原子发布**：持全局 `RwLock` 写锁重读当前容器状态——已回收返 `404`；仅 `tags` 变更正常发布并保留新标签；版本不符、路径被占或上传期间容器发生移动/改名返 `409`。校验通过后：安装容器元文件并刷盘暂存目录 → 写全局 `.zxs/inflight/<tx_id>.json` → 首版补齐祖先与容器目录 → 本盘原子 `rename` 发布 `v00000N` → 原子替换 `<容器>/.zxs` → 提交 SQLite → 删 `inflight`。

#### 2. 同盘与跨盘移动（`PATCH /containers/{id}`、`PATCH /folders`）

* **同物理数据盘（`src_disk == dst_disk`）**：若源与目标规范拼写完全相同、目录存在且未冻结则直接返回。否则在写锁内校验合法性（排除自身以支持纯大小写改名；整树移动限于盘内二级及以上子目录、禁止移入自身、子孙路径总长 `≤ 1024` 字节）→ 写 `inflight` → 补齐目标父目录 → 单次本盘 `rename`（打开中的流播句柄不受影响，容器 `.zxs` 随目录移动）→ 单事务更新 SQLite 路径前缀 → 删 `inflight`。
* **跨物理数据盘（`src_disk != dst_disk`）**：
  1. **冻结与预拷贝**：单容器移动先读源摘要；跨盘目录迁移一次只执行一项，先核对源/目标可用，再冻结与源树相交的新写入并返回 **`423 Locked`**（上级目录操作及搬入/搬出均视为相交；目标路径亦参与写入登记，确保目标回收等待已受理搬入），等待在途相交写入完成（源树仍可读，其余路径照常写）。整树迁移复制前核对盘上与索引源容器集合，有差异即报错保留源树。随后经 `std::fs::copy` + `sync_all` 拷至 `<dst_disk>/.zxs/staging/<tx_id>/tree/`，目标盘满返 `507`。
  2. **短暂写锁切换**：持写锁复核目标可用（单容器同时复核源状态并更新副本容器 `.zxs`）→ 写并刷盘 `inflight` → 补齐目标父目录 → 源盘 `rename` 将原目录移入 `<src_disk>/.zxs/purging/<tx_id>/` → 目标盘 `rename` 发布副本 → 单事务更新 SQLite → 删 `inflight` → 释锁、解除禁写并清扫暂存与待删目录。目标在复制期间被占返 `409`。

#### 3. 多回收盘选盘、还原与彻底清理

* **选盘与移入回收站**：按在线回收盘 `statvfs` 剩余空间为每个容器选最大可用盘（容量取版本清单字节之和，整树批量逐项扣减估算余额）；无在线盘或空间不足返 `507` 并注明原因。
* **预拷与发布**：完整副本写入 `<RECYCLE_DIR>/<回收盘>/.zxs/incoming/<tx_id>/<rid>/` 并刷盘容器树，再写批次元文件 `.zxs` 并刷盘目录。单容器回收校验可选 `expected_version` 并在发布前复核源状态；整树回收先冻结源树相交写入、等待在途写入结束、核对盘/索引容器集合，全部预拷完成后将整树计划写入 `inflight/<tx_id>.items/plan.json` 并刷盘。发布阶段共用统一顺序：写并刷盘 `inflight` → 源移入 `<src_disk>/.zxs/purging/<tx_id>/` → 原子重命名发布各回收批次 → 每盘统一刷盘涉及的两个父目录 → 提交 SQLite → 删 `inflight` → 释锁清盘。
* **整树删除边界**：`DELETE /folders?recursive=true`（仅限盘内子目录）将子树各容器拆为独立回收项，纯空子目录随源树删除；还原单容器时在目标盘自动补齐祖先目录。
* **还原与彻底清理**：还原先从回收盘复制到目标数据盘暂存区并刷盘，再持写锁确认目标无冲突 → 写 `inflight` → 原回收批次移入 `<rec_disk>/.zxs/purging/<tx_id>/` → 目标盘 `rename` 发布并更新 SQLite → 删 `inflight` → 释锁清盘。彻底删除或清空回收站（范围为当前在线回收盘）同样先移入各盘 `.zxs/purging/`、统一刷盘两父目录再释锁清盘。

#### 4. 原件完整性校验

跨盘移动、回收与还原均在目标暂存区核对**每一版原件的长度与 BLAKE3**。跨盘移动与回收还将磁盘版本清单与索引记录交叉核对；还原使用已校验的版本清单更新索引。任一环节发现损坏均保留原副本，暂存区由守卫清理。

### 3.5 启动恢复、离线盘处理与索引重建

1. **持锁与在线盘识别**：获取全局 `.lock`，逐项核对 `config.toml` 中已登记盘的 `.zxs/disk.toml`。缺少身份文件的盘视为**离线**并跳过其路径；存在身份文件时强校验 `kind` 与 `disk_id`，挂错盘直接报错。
2. **在途日志前滚恢复**：
   * 同盘文件/目录已移动：补刷盘新旧父目录后结束事务；
   * 跨盘切换目标已发布：因旧路径在发布前已移入 `purging/`，即使源盘离线也可完成恢复；
   * 尚未发布且后续步骤依赖离线盘：保留日志与暂存，待离线盘接回重启后再完成；
   * 清空回收站重放仅处理原记录中仍在线的盘，离线盘接回后清理其 `purging/` 中的残留批次，未移出归档保留。
3. **并行重建在线盘索引与清扫**：每次启动均并行扫描在线数据盘与回收盘（跳过盘根 `.zxs/`），在单次 SQLite 事务中重建索引——数据盘校验容器保留范围、版本元数据与原件长度，回收盘扫描一级回收批次。无在途日志时，提交前用新扫描结果核对旧索引中在线盘的版本与回收批次，缺项则报错并保留原索引。恢复完成后，仅清扫在线盘的 `.zxs/{staging,incoming,purging}` 与系统盘遗留计划，不触碰源盘普通目录文件。

---

## 四、HTTP/1.1 通信契约（8 组接口）

### 公共约定

* **统一请求头鉴权**：除 `POST /sessions` 外，全接口（含 `/payload` 媒体流播）统一且仅校验 `Authorization: Bearer <session_token>`，**彻底禁止 URL `?token=` 传参**（桌面 CLI、桌面 UI 本机反代、iOS `AVURLAssetHTTPHeaderFieldsKey`、Android `VideoView.setVideoURI` 均原生支持）。
* **流式上传请求头**：控制参数经 `Zxs-Params: <Base64URL-no-pad(UTF-8 JSON)>` 传递，正文为 `application/octet-stream`，必带 `Content-Length: <u64>`，可选 `Zxs-Blake3: <64-hex>`（可编辑散文件先算摘要随头发送由服务端发布前强校验；私有暂存目录包可单遍流传并核对回执；均支持 `0` 字节空文件）。

### 8 组接口总表

| # | 接口 | 核心语义与状态码 |
| :-: | :--- | :--- |
| **1** | `POST /sessions`<br>`DELETE /sessions/current` | Argon2id 校验口令签发永不过期的 256-bit 内存 Bearer Token；主动登出销毁当前内存会话并清除本地保存的密码（重试遇 `401` 视为已登出）。 |
| **2** | `POST /containers` | `Zxs-Params: { folder, name, tags }`（禁止 `folder == "/"`），流式落目标盘暂存并发布 `v1`；冲突返 `409` 并提示已占用路径的实际拼写。 |
| **3** | `POST /containers/{id}/versions` | `Zxs-Params: { expected_version }`，流式发布 `latest_version + 1`；版本不匹配或上传期间容器移动/改名返 `409`。 |
| **4** | `PATCH /containers/{id}` | JSON `{ folder?, name?, tags? }`（目标禁止 `"/"`）；无实际变化直接返原摘要且保留 `updated_at`；同盘单次 `rename`，跨盘自动预拷切换（盘满返 `507`）。 |
| **5** | `GET /containers`<br>`GET /containers/{id}` | • 列表分页：`?folder=&recursive=&tag=&q=&cursor=&limit=` 按 `(folder_key, name_key, id)` 升序返 `{ containers, next_cursor }`（`folder` 默认精确筛选，`/` 返空；配 `recursive=true` 含子树；省略则跨全盘检索；查询按 NFC+小写归一化；摘要含 `latest_blake3` 不含 `size`；翻页改名重复 ID 时前端以新项替换旧行）。<br>• 批量核对：`?ids=<id1>,<id2>`（单独使用，最多 100 个，缺项省略）。<br>• 详情：返回容器与升序 `versions`（含各版 `size` 与 `blake3`）。 |
| **6** | `GET` / `HEAD` `/containers/{id}/versions/{v}/payload` | 读锁内一次查询路径与版本并打开句柄后释锁；同步 4 MiB 缓冲提供全量下载与单区间 `Range`（小响应走独立通道），附强 `ETag: "<blake3>"` 与 `nosniff`。 |
| **7** | `DELETE /containers/{id}[?expected_version=<u32>]`<br>`GET /recycle`<br>`POST /recycle/{recycle_id}/restore`<br>`DELETE /recycle/{recycle_id}` & `DELETE /recycle` | • 回收：按最大剩余空间选在线回收盘；支持 `expected_version`（变更返 `409`）；无在线盘或满返 `507` 并注明原因。<br>• 列表：跨在线回收盘按 `(recycled_at DESC, recycle_id DESC)` 分页（省略源目录）。<br>• 还原：必填 `folder`，可选 `name`（默认回收项名），冲突返 `409`。<br>• 清理：彻底删单条或清空当前在线回收盘。 |
| **8** | `GET /folders?folder=<path>[&recursive=true]`<br>`POST /folders`<br>`PATCH /folders`<br>`DELETE /folders?folder=<path>[&recursive=true]` | • 查询：`folder=/` 列出在线一级数据盘；盘内列直接子目录（含空目录，无 `count`）；加 `recursive=true` 返整棵后代目录树。<br>• 新建：补齐盘内缺失祖先，同拼写已存在返 `200`。<br>• 移动：`{ from, to }`，限于盘内子目录、禁移入自身、子孙限 1024B；同盘 `rename`，跨盘先冻结源树（相交写返 `423 Locked`）再预拷切换。<br>• 删除：默认仅删空目录；`recursive=true` 将子树容器拆项回收并清除目录树。 |

---

## 五、跨平台客户端与交互规范 (`zxs-client`)

### 5.1 桌面双入口（CLI 与本机 Web UI）与传输进度

* **合一单二进制 `zxs-cli`**：
  * **CLI 命令集**：`login`、`logout`、`ls`、`mkdir`、`mv-folder`、`rmdir [--recursive]`、`detail`、`patch`、`checkout`、`status`、`sync`、`refresh`、`untrack`、`commit`、`init-bundle`、`recycle [ls | purge]`、`restore`、`ui`。
  * **CLI 列表与等待**：`ls <目录>` 仅列所选目录的文件与直接子目录（支持 `--q`、`--tag` 筛选文件），省略目录时全盘检索；跨盘移动、回收、还原等长写操作等待服务端完成并返响应头（回执正文保留读取时限）。
  * **工作区选择**：保存用户选定的已有目录路径；整体迁移工作区通过系统文件管理器完成后，在界面重新选择新路径即可。
* **本机浏览器 UI（`zxs-cli ui`）**：
  * 监听 `127.0.0.1:0` 随机端口，一次性 `#secret` 换取 `HttpOnly; SameSite=Strict` Cookie，强校验 `Host` 与 `Origin`，托管内嵌零构建 ES2024 页面并唤起系统浏览器；音视频预览经本机 `/media/` 反代由 Rust 附带 Bearer 头。
  * **状态隔离与防竞态**：所有请求绑定发起时的工作区与服务器上下文（断连丢弃旧列表，迟到回执/告警/冲突/确认框不污染新工作区或覆盖后打开表单，切换目标即关旧确认框）；历史恢复冲突确认绑定最近一次请求并标明版本号；手动刷新与目录同步后同步刷新已开详情；远端目录被移走时退回 `/` 并提示。
  * **表单克制提交**：文件移动/改名仅提交改动字段；目录移动或标签集合无实际变动时直接关闭弹窗。
* **文件传输进度与取消**：
  * 按实际载荷字节实时采样完成比例与速率（目录提交、相册与同步批次逐件更新；准备、校验与安装阶段沿用操作状态，以最终结果定成败）。
  * 终端写入交互式 `stderr`，桌面网页从回环服务读取进度，移动端经同步 C ABI 回调更新原生进度条；点击下载进度右侧 `×` 可在读流中取消当前下载并清理未完成文件。

### 5.2 本地工作区 (`workspace.sqlite`) 核心机制

#### 1. 默认镜像、拼写与内容保护

* `workspace.sqlite` 对 `container_id` 与 `local_path` 设双 `UNIQUE` 约束。
* 未指定 `--to` 的默认检出落盘至 `<workspace>/<数据盘>/<子目录>/<name>`（目录包带 `.zip` 后缀）；自定义路径检出始终沿用已追踪路径（即便后来与镜像路径重合）。
* 首次 `commit` 默认取相对父目录（散落工作区根目录下的文件拒绝提交，提示移入 `<数据盘>/` 或显式传 `--folder /<数据盘>`）。
* 移动、改名与回收站还原只跟随受影响的本地 `clean` 追踪项；空目录由「同步此目录」建立；已有父目录沿用本机 NFC 等价拼写；区分大小写文件系统上若存在仅大小写不同的目录或文件冲突则报错保留本地；纯大小写改名经暂存原子无覆盖移动。
* **覆写/搬移硬前提**：执行任何本地覆写或自动搬移前，除 `(size, mtime_ns)` 外**必须复核本地 BLAKE3**，内容有变立即停止并提示用户。

#### 2. 历史版本恢复到工作区

* 网页详情栏历史版本每行提供「恢复到工作区」，CLI 使用 `checkout <id> --version <N>`（迟到提示按原上下文显示）。
* 桌面与移动端按**历史版 `name_at_commit` 或当前版容器现名**判断是否按 ZIP 识别（识别为目录包时，桌面要求目标目录带 `.zip` 后缀）。
* 恢复旧版记为本地待提交修改（已有追踪项沿用原提交基线，首次恢复以当时读到的远端最新版为基线），`sync` 保留待提交内容，显式 `commit` 正常走版本前置校验发布新版；选择当前版按普通检出登记。
* 同位置已追踪干净文件或重复恢复同一普通历史版时，BLAKE3 一致直接复用；仅改容器名称大小写时显式检出沿用已有本地路径拼写（含普通 ZIP 与目录包互转）。

#### 3. 同步预检、安全替换与中断恢复

* **预检**：以 `(container_id, version, blake3)` 判断更新（版本倒退或同号异哈希报冲突保留本地）；最新版复用列表版本与哈希并流式校验正文；本地已修改与普通文件路径冲突在下载前拦截（ZIP 路径在整包校验识别后核对）；默认检出/同步遇同名未追踪目录、或下载后原追踪路径已消失时均报冲突保留现场。
* **安全安装**：有新版时先暂存校验，再将待替换旧件同盘移入备份位置、无覆盖安装新件。目录包按目标现有成员计算替换并保留被忽略项；遇到下载期间已追踪成员被删、新忽略规则将原排除项纳入提交、替换触及受忽略保护项、待移除目录仍有需保留内容、或双拼写未指向同一实际路径时，均停止报冲突。
* **中断记录与恢复**：发布前在 `workspace.sqlite` 按容器记录所选版本、BLAKE3、提交基线、备份位置及镜像/自定义标记；中断重试必核对原版本 BLAKE3 并沿用文件/目录包类型，记录不完整则报中断保留现场与备份；成功后清中断记录（自动同步清理临时旧件，失败/中断及用户显式确认的备份保留）；中断项暂停同步删除。

#### 4. 工作区扫描、显式目录提交与同步

* **`status` 扫描**：`(size, mtime_ns)` 并行比对已追踪项并发现未追踪新增文件/目录包（跳过 `.zxs/`、符号链接与目录包忽略项；未追踪设 **1,000 条熔断上限**并标 `truncated`），统一输出 `clean / modified / missing / interrupted / untracked` 及非法 `.zxsignore`/符号链接告警；Web UI 仅扫描当前目录子树并按该子树计数。
* **显式目录提交**（仅在网页「工作区」显示）：先预检数据盘一次再扫描子树；跳过同级规范名冲突子树，报告无效目录包规则并继续其他项；复用远端子目录清单仅补建缺失空目录；**复核 `(size, mtime_ns)` 未变追踪项的 BLAKE3** 以捕捉同大小同时间编辑（普通文件校验未变时复用已算 BLAKE3 供上传）。
* **批量同步**：`sync` / `refresh` 默认按 `ids` 批量同步已追踪项；`sync --folder <目录>` 或「同步此目录」一次拉取子树目录再按子树分页检出容器；自动优先处理可为后续移动腾位置的容器，路径循环占用报冲突保留原件；逐项报告结果，任一失败返回非零退出码。

#### 5. 丢包认领、冲突处置与带版本保护的同步删除

* **预检与认领**：提交先读摘要预检基线与镜像路径（远端位置已变默认暂停并展示双路径）；丢响应重试遇 `409` 时重查摘要，同容器或同路径最新版 BLAKE3 与本地一致即自动认领（复用同一次本地 BLAKE3；另存仅匹配新路径；返回完整回执或拉取时读详情）。
* **冲突处置**：三选一——追加为 `latest_version + 1` / 另存新文件 / 备份本地后拉取远端最新版。确认操作复核原容器 ID（首次提交遇同名文件亦复核首次容器 ID，对象变化则提示重提）；单个冲突弹窗一次只执行一种处置，失败可重选；冲突已在别处解决时旧「拉取」确认提示刷新。
* **同步删除与远端 `404`**：本地缺失条目支持重新检出或同步删除，**同步删除必传本地认知的 `expected_version`**（远端更新返 `409` 强制二次确认）；远端已删（`404`）保留本地文件且仍可打开，可显式 `untrack` 转为未追踪文件或去回收站还原。

### 5.3 `.zxsignore` / `.zip` 目录包规范

* **完整名称与共存边界**：目录 `abc` 与容器 `abc.zip` 均使用完整名称，可同目录共存；新检出目录包以完整 `.zip` 名落盘为文件夹。`init-bundle` 以最终 `.zip` 名称预检本地/远端占用与追踪重叠，在原目录写 `.zxsignore` 并补齐缺失的 `.zip` 后缀（保留 `.zip`/`.ZIP`）；禁止在已有 `.zip` 目录包内嵌套初始化；提交、检出、移动及网页还原已识别目录包均强制 `.zip` 后缀（大小写不敏感）。远端丢失 `.zip` 后缀时保留本地并报错，须恢复后缀后方可认领。若同步/移动/恢复会把独立文件或普通目录放进未追踪本地目录包、或恢复普通 ZIP 时同路径本地目录包仍含被忽略内容，均报冲突保留原目录。扫描忽略与独立追踪项重叠的手工 `.zxsignore` 并告警。
* **文件移入 / 移出目录包（零监听、零猜谜）**：
  * **移入目录包**：原独立追踪项标为 `missing`（提示可带版本核对同步删除原容器，或重新检出）；目标目录包标为 `modified`（提交时并入新版）。
  * **移出目录包**：原目录包标为 `modified`（提交新版移除该成员）；外部文件自然标为 `untracked`（提交时创建全新独立容器）。
* **确定性打包与安全解包**：
  * 打包按 NFC 归一化路径与 `.zxsignore` 过滤（解析规则忽略开头 UTF-8 BOM，ZIP 内保留清单原始字节与非忽略空子目录 `dir/`，跳过符号链接）；**第 0 条目（偏移 0 字节）必须精确为普通文件 `.zxsignore`**（缺失则拒提）；其余条目按 NFC UTF-8 字节序排列、固定归零时间；`≤ 4 MiB` 条目按 `40–64 MiB` 有界窗口由 `rayon` 并行 `zlib-rs` Level-1 压缩合并，`> 4 MiB` 单路流写（超限自动 ZIP64）。
  * 桌面浏览复用本地已知追踪类型，检出所选 ZIP 时一次下载并校验整包后本地检查 `.zxsignore`；解包强校验归档结构与常规文件属性，实施 **Zip Bomb 防护**（条目上限 `200,000`、解压总字节默认上限 `64 GiB`、解包前校验目标盘剩余空间、拦截 `..` 与符号链接穿越），绝不触碰本地忽略文件。

### 5.4 移动端原生薄壳 (`crates/ffi` + iOS 17+ / Android 13+)

* **浏览、ZIP 工作区与双有界缓存**：
  * `browse` 在根 `"/"` 仅请求 `GET /folders`，盘内读取目录与文件元数据，ZIP 先按压缩包展示；点击打开时才下载校验整包原件（历史版按 `name_at_commit`、当前版按容器现名判断 ZIP 与系统预览类型）。
  * Rust 在本机识别目录工作区并索引一次目录，翻页与打开成员复用本机索引与原件，显式刷新成功后再切换当前目录索引，返回文件列表成功后释放索引；普通 ZIP 直接交系统预览。
  * 原件缓存与解压成员缓存各自独立维护，常规上限各为 **64 份 / 512 MiB**；重开时校验同名本地字节（预览名变更且无同名缓存时重拉一次）。
* **音视频流播与凭据管理**：
  * 普通音视频 `open` 返回 `/payload` URL 与当前 Bearer 头（使用提交时媒体类型），直交 iOS `AVPlayer` / Android `VideoView` 原生流播。
  * 仅在登录确认口令错误时清除已存凭据；主动退出清除本地凭据并最多用 2 秒尝试一次远端注销；iOS 存 Keychain，Android 存 Keystore AES-256-GCM 并接入 `OnBackInvokedCallback`。
* **文件、版本与相册上传**：
  * 数据盘及子目录右下角提供「上传」菜单（上传文件 / 上传照片和视频）；已有文件更多菜单提供「上传新版本」（单选，按容器 ID 与当时版本号提交）；ZIP 整件上传。App 逐项暂存私有临时目录，处理完即释放；单失败项可单独重试（来源失效提示重选）。
  * 复用桌面冲突查询与 BLAKE3 认领规则：同内容认领，异内容逐项提示追加（用确认版本号）/ 改名另存 / 跳过；原容器已被回收、源名或另存名不合规时均支持当前批次改名重试；手工另存严格使用输入名。
  * **相册上传防重名**：按 `name`、`name (2)`、`name (3)`… 先查同名候选比对 `latest_blake3`（摘要相同须按目录拼写与文件名复查仍匹配才跳过）；需上传时才读子目录避让占用名并取首个空闲名；单项失败不阻断同批其余项。

### 5.5 跨端 UI 审美与交互硬约束（静谧精密风格）

| 维度 | 硬约束 |
| :--- | :--- |
| **品牌标识** | 橙色文件夹、存储与历史图案统一用于 iOS/Android 应用图标、Windows 客户端内嵌图标与开始菜单/应用列表，以及桌面网页标签页和侧栏。 |
| **色彩与主题** | 源码禁止裸写 hex，统一语义 Token（`--bg-canvas/surface/elevated/subtle`、`--border-default/subtle`、`--text-primary/secondary/muted`、钴蓝 `--accent`、状态色 `--ok/warn/danger`），原生自适应暗黑模式。 |
| **排版与网格** | 系统无衬线 + 等宽字体栈；严格 **6 级字阶 × 3 字重（`400/500/600`）**；**4px / 8px** 间距网格与同心圆角（`12 / 8 / 6 / 5px`）；版本、大小（`KiB/MiB/GiB`）、时间（`YYYY-MM-DD HH:mm`）启用 `tabular-nums`。 |
| **图标与反馈** | 禁用 Unicode 杂项符（桌面用 `16×16` `1.5px` 内联 SVG，iOS 用 SF Symbols，Android 用 VectorDrawable）；禁用 `window.confirm`/`alert`；每视觉域最多 1 个主按钮；桌面传输进度与普通提示在左侧栏底部占位（窄窗置顶栏下方）。 |
| **列表与交互** | 列表零缩略图且仅展 `GET /containers` 字段（`size` 仅在详情与历史显示）；文件行操作列提供移动/改名与回收（回收弹窗示完整路径）；目录浏览与全盘搜索解耦（根 `"/"` 展示一级磁盘图标并隐藏改名/删除，删非空目录弹窗提示进回收站）；已识别目录包使用完整 `.zip` 名；显式警示非法 `.zxsignore`、符号链接、`missing` 与 `404` 追踪项并提供一键处置；桌面支持 `/`, `↑/↓`, `Enter`, `Backspace`, `Esc` 键盘流与宽屏推挤式 Inspector。 |

---

## 六、离线运维与独立磁盘准备工具

### 6.1 服务端离线运维子命令 (`zxs`)

所有离线命令均须获取 `<DATA_DIR>/.zxs/.lock` 排他锁（在线服务运行中直接报错拒改盘），并核对所需已登记盘的 `.zxs/disk.toml`。

| 子命令 | 核心职责与关键行为 |
| :--- | :--- |
| `zxs serve [--data-dir] [--recycle-dir] [--listen]` | 默认监听 `0.0.0.0`，启动恢复后只索引在线盘；收到 `SIGTERM` / `Ctrl+C` 时停收新请求、完成已开始同步磁盘操作、清理未完成上传暂存并释放库锁。 |
| `zxs init --data-dir <p> --recycle-dir <p>` | 登记由 `zxs-prepare` 准备就绪的一级盘目录（数据盘与回收盘各至少 1 个），写入 `<DATA_DIR>/.zxs/config.toml`。 |
| `zxs sync-disks` | 先恢复在途事务再增量登记已准备的新盘（无新盘直接返回；回收根离线时仍可登记新数据盘并保留原回收盘登记）；扫描在线盘核对缺项，索引损坏时复用启动重建。 |
| `zxs reset-password` | 持全局锁更新 `config.toml` 中 Argon2id 哈希，使所有旧客户端凭据失效。 |
| `zxs import <src_dir> --folder <target>` | 本机 NVMe 多线程单遍哈希导入至指定数据盘目录（保留空目录、跳过符号链接、预扫描拒同级规范名冲突与路径超限、只登记叶目录并补齐祖先、原件设 `0444`）。 |
| `zxs prune --keep-last <N> [--container <cid>]` | 强制 `N >= 1`；先校验参数与容器 ID 再开索引，从索引筛候选容器并在删前缀前复核盘上版本。 |
| `zxs verify` | 并行核对在线数据盘与回收盘每版原件长度与 BLAKE3；发现待恢复正式事务日志时提示先运行 `rebuild-index`；报告缺元文件的拟回收批次目录；旧索引可读时交叉核对缺项。 |
| `zxs rebuild-index` | 按启动流程恢复在途日志并从在线盘重建索引（SQLite 明确损坏时自动重建）；无在途日志且旧索引可读时核对缺项；无索引时按容器保留范围查缺版（整容器连元数据一并丢失须依赖旧索引或外部记录发现）。 |

### 6.2 独立磁盘准备工具 (`tools/zxs-prepare`)

`tools/zxs-prepare` 在 Linux、macOS 和 Windows 提供统一 CLI 与本机浏览器 UI（详见 [操作说明](tools/zxs-prepare/README.md)），用于在接入服务端前完成空盘初始化或存量数据盘规范化转换：

* **空盘准备**：为已有空目录写入数据盘或回收盘 `.zxs/disk.toml`（网页改路径后须重新读取才可准备，空盘默认选中数据盘；对已准备盘重跑 `init` 提示已准备，存在中断记录提示运行 `resume`）。
* **存量数据预检与打包**：可逐项指定现有目录是否打包为 `.zip` 目录包（CLI `--within` / `--bundle` 接受末尾单分隔符）。复制前校验显式选择存在、无父子路径重叠、成员合法性、补 `.zip` 后命名与同层规范名冲突、NFC 归一化完整逻辑路径 `≤ 1024` 字节及解压上限；`.zxsignore` 置首项并忽略开头 UTF-8 BOM（规则与成员名均 NFC 归一化，清单编码错示实际路径，ZIP 保留原始字节）；普通文件生成首版容器，保留目录层级与空子目录。
* **本盘暂存两阶段安全切换**：持本盘独占锁暂存完整结果并记录切换意图；首次准备或中断 `resume` 时依次执行：刷盘记录目录 → 原目录树移入 `.zxs/originals/` 并刷盘 → 成批发布结果并刷盘暂存与盘根 → 写入 `.zxs/disk.toml`（盘身份已发布则仅清准备记录）。服务端 `init` 与 `sync-disks` 只登记已发布身份文件的盘。

---

## 七、工程规范与静态门禁

### 7.1 四层单向依赖

严格按 **`接入外壳层 (api / cli / ui) → 用例编排层 (service) → 基础设施层 (store / transport) → 纯领域层 (domain)`** 自上而下调用。物理路径拼装只允许写在 `store/layout.rs`，SQL 只允许写在 `store::db` 模块，`domain` 层零 I/O。

### 7.2 代码规模预算与模块红线

| 维度 | 硬红线 | 校验方式 |
| :--- | :--- | :--- |
| **服务端业务代码总量** | **≤ 5,500 行** | `cargo fmt` + `python3 tests/check_line_budget.py`（统计非空非纯注释行，排除测试、第三方、配置与生成资源；自研子 crate 计入所属项目；`zxs-prepare` 独立统计；超限优先删重复与冗余分支） |
| **客户端业务代码总量** | **≤ 12,000 行**（含 Rust、CLI、Web、Android、iOS） | 同上 |
| **单文件 / 单函数 / 结构** | 单文件 **≤ 350 行**（不含测试）、单函数 **≤ 60 行**、最大缩进 **≤ 3 层**、函数参数 **≤ 4 个** | 人工复核 + CI 门禁 |
| **模块组织** | Rust 统一用 `foo.rs` + `foo/bar.rs`（**禁止新建 `mod.rs`**）；桌面 Web 用原生 ES Modules | 同上 |

### 7.3 强类型、静态门禁与资源边界

* **强类型与严格反序列化**：入口立即解析为 `ContainerId`、`DiskName`、`FolderPath`、`FileName`、`VersionNum`、`Blake3Hash`；状态机用互斥 `enum`；所有 JSON/TOML 输入结构体标 `#[serde(deny_unknown_fields)]`。
* **编译静态门禁**：根 `Cargo.toml` 固化 `unsafe_code = "forbid"`（仅 `crates/ffi` 允许隔离 FFI `unsafe`），`deny` 全部 `unwrap_used`、`expect_used`、`panic`、`todo`、`dbg_macro`；CI 开启 `-D clippy::pedantic -D warnings`；错误链统一用 `thiserror`。
* **RAII 守卫与锁序**：临时目录统一由 `StageGuard`、`TempDir` 或 `DropGuard` 托管。跨盘目录迁移与整树回收共用进程内路径登记，等待在途相交写入时释放登记锁与全局发布锁。全局 `RwLock` 读锁用于下载打开句柄与按 ID 定位路径，写锁用于发布与跨盘切换（锁内按本盘 `rename` → SQLite 写事务顺序执行）；收流、校验、跨盘复制与清盘全在锁外完成。
* **有界内存**：正文严禁整文件读入内存，统一固定 **`4 MiB`** 缓冲流；单份元数据文件读取上限 **`1 MiB`**；可含前后两份元数据的在途日志读写上限 **`2 MiB + 4 KiB`**。
* **统一词表与测试门禁**：全栈统一使用 `Container`、`Version`、`Folder`、`LocalPath`、`Recycle` 五个核心名词；服务端、客户端与准备工具各自维护 Rust 单元测试，由 `tests/smoke.py`（多数据盘 + 多回收盘）与系统集成测试覆盖准备中断、盘身份交接与三端路径边界。

---

## 八、性能目标与交付里程碑

### 8.1 可测端到端性能与内存目标（目标网卡 + NVMe 实测）

| 指标 | 目标值 |
| :--- | :--- |
| **单流读写吞吐** | 跑满物理链路与目标 NVMe 上限：10GbE **1.1+ GB/s**；25GbE / 40GbE / 雷电直连 **2.5 – 4+ GB/s**（多客户端访问不同物理 NVMe 时吞吐线性叠加） |
| **1 GB 代码库打包上传** | **< 1.5 秒** |
| **50 万容器索引指标** | 冷启动 **< 50 ms**；多盘并行重建索引 **< 3 秒** |
| **进程常驻缓冲内存** | **< 128 MiB** |

### 8.2 实施里程碑

| 阶段 | 子项目 | 交付范围与准出标准 |
| :---: | :---: | :--- |
| **S1** | `zxs-server` | 完成 `vendor/` 剪裁、容器/版本/回收 `.zxs` 元文件、`init`/`sync-disks` 登记与盘身份识别、一级盘与 NFC 路径校验、`0444` 原件、本盘流式发布、同盘/跨盘移动与 `423` 冻结、多盘选盘回收与指定目录还原、在线盘 SQLite 索引、启动恢复、8 组 HTTP 接口及全部离线运维子命令。 |
| **C1** | `zxs-client`（Core & CLI） | 完成独立 `vendor/`、纯同步 `crates/core` 与完整 `apps/cli`；联调多盘服务端跑通永久密码与重启静默重登、默认镜像与自定义路径、覆写前 BLAKE3 核验、`interrupted_checkout` 恢复、带 `expected_version` 同步删除、散文件预算 BLAKE3 与丢包认领、全工作区扫描（熔断与告警）、完整 `.zip` 目录包确定性打包、整包校验识别与防 Zip Bomb 保护。 |
| **C2** | `zxs-client`（Web UI） | 在 `apps/cli` 完成 `ui` 子命令（`127.0.0.1` 回环与 `/media/` 反代 + 内嵌零 npm 原生 ES2024 页面，满足 §5.5 全部审美、暗黑模式、根盘视图、目录包展示、目录管理与回收站清理规范），三大桌面浏览器实测文件管理、目录提交、唤起本地编辑与原片流播。 |
| **C3** | `zxs-client`（Mobile） | 完成 `crates/ffi` 与 iOS 17+ / Android 13+ 原生薄壳，支持文件多选与显式版本提交、冲突处理、当前目录相册上传与逐项重试、原生音视频流播、ZIP 整包收发与本地浏览、Keychain / Keystore 凭据保存，完成局域网实机验收。 |
