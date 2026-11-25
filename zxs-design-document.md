# ZXS 总体架构与系统设计规格书

> **文档定位**：本文是 ZXS 从零全新实现的唯一总体工程规格书。面向**个人超高速局域网（10GbE / 25GbE / 40GbE / 雷电直连）+ 全 NVMe 固态存储 + 纯现代 64 位软硬件栈**，管理**办公文档、照片、视频、代码/文本项目、普通文件**。全系统拆分为 **`zxs-server`（仅现代 Linux）与 `zxs-client`（跨桌面与移动端）两个独立子项目**，依赖互不相通；**不做历史兼容、不搞协议版本号、不写特殊分支处理**，协议变更时两端同步升级。

---

## 一、目标基线、设计原则与反过度设计红线

### 1.1 现代软硬件基线（零兼容包袱）

| 维度 | 支持基线 | 工程收益 |
| :--- | :--- | :--- |
| **CPU 指令集** | **x86_64**：`x86-64-v3`（AVX2 + BMI2 + FMA + PCLMULQDQ，自建可开 `target-cpu=native`）<br>**ARM64**：Apple Silicon（M1+）/ ARMv8.2-A+（NEON + 硬件 CRC32） | 默认全开 SIMD 向量化，BLAKE3 哈希与客户端 ZIP 压缩无标量慢速回退；不构建任何 32 位或旧架构产物。 |
| **服务端 OS** | **仅现代 Linux 6.6+ LTS**（`x86_64` / `aarch64`） | POSIX 语义一致（打开流读取期间可直接原子重命名或删除目录），免除跨系统服务端兼容负担；多盘扩容与条带化直接交由 Linux 内核卷管理（LVM / mdadm / ZFS / Btrfs）。 |
| **桌面客户端 OS** | **Linux 6.6+** · **macOS 15+**（Apple Silicon） · **Windows 11 24H2+**（`x86_64-msvc`） | 单一纯同步客户端二进制（含 CLI 与本机 `127.0.0.1` 浏览器 UI），零重型 GUI 框架依赖。 |
| **移动客户端 OS** | **iOS 17+**（`aarch64-apple-ios`） · **Android 13+**（`aarch64-linux-android`） | 通过约 200 行零依赖 C-ABI（`crates/ffi`）接入轻量原生壳，覆盖相册备份、媒体流播与文档查看。 |
| **语言基线** | **Rust Edition 2024**（稳定版 1.89+） | 原生支持 `LazyLock`、`File::try_lock` 等现代标准库能力，淘汰过渡期胶水包。 |

### 1.2 五条核心原则

1. **双项目独立、同步升级零版本包袱**：`zxs-server` 与 `zxs-client` 代码与 `vendor/` 互不相通；**不设 URL 版本号、不设数据格式版本字段、不留旧兼容代码**，变更时两端一并升级。
2. **单数据根 + 单回收根、裸盘双击即开**：服务端只管一个 `--data-dir` 与一个 `--recycle-dir`；每个文件对应一个文件夹，内部按 `v000001`、`v000002` 单调递增存放原件（`payload.<ext>`）与 `version.json`，SQLite 仅作可随时重建的加速索引。
3. **单请求原子流上传、零暂存池包袱**：依托万兆+局域网与 NVMe 的数 GB/s 线速，所有上传采用**单次 HTTP 流式直写 `.staging` → 校验 BLAKE3 → `sync_all` → `rename` 发布**，断连即由 `Drop` 自动清理，不维护断点续传暂存表。
4. **单调线性版本 + 专用回收目录**：单线递增版本号（`prune` 清理旧版后允许稀疏，新版本号永远为 `latest_version + 1`）；删除整容器迁入专用 `--recycle-dir`，随时还原。
5. **服务端纯粹单文件 + 客户端 `.zxsignore` 整包收发**：服务端不知晓也不解析 `.zip`；客户端识别含 `.zxsignore` 的本地目录自动多核打包为标准 `.zip` 整包上传、整包检出，普通目录直接编辑。

### 1.3 反过度设计红线（明令禁止项）

* **禁止应用层卷管理**：不做多数据盘路由表、配额计算、跨盘迁盘或掉盘降级状态机。
* **禁止分块断点续传状态机与 `multipart`**：不建 `uploads/` 暂存池、不维护偏移锁与过期回收线程。
* **禁止重型桌面 GUI 框架（如 Tauri / Electron / egui）**：桌面交互统一由 `zxs-client ui` 启动本机 `127.0.0.1` 回环服务并打开系统浏览器实现，省去约 300 个跨平台窗口绑定包。
* **禁止远端 `.zip` 局部偏移拆包**：`.zxsignore` 目录包一律整包高速流式上传与下载；HTTP `Range` 仅用于服务端响应大视频拖动播放。
* **禁止在客户端核心引入异步运行时**：客户端核心统一采用纯同步 I/O + `ureq` 连接池 + `rayon` 线程池。
* **禁止协议版本协商、复杂分支图、后台自动同步守护进程、内置文档编辑器及未到阶段的空壳模块**。

---

## 二、双独立子项目结构与极简依赖治理

### 2.1 目录结构

```text
/
├── README.md                      # 总索引与 HTTP 协议契约说明
├── zxs-server/                    # 【子项目 1：Linux 存储服务端单二进制】
│   ├── .cargo/config.toml         # 锁定 ./vendor 离线源与现代 CPU 编译参数
│   ├── Cargo.toml                 # 单包工程（forbid(unsafe_code)）
│   ├── Cargo.lock
│   ├── VENDOR.toml                # 上游包版本、许可证与原始包 SHA-256
│   ├── VENDOR.sha256              # 剪裁后 vendor/ 逐文件哈希清单
│   ├── tools/sync_vendor.py       # 独立 vendor 同步与剪裁脚本
│   ├── vendor/                    # 服务端第三方源码（零压缩库、零 GUI 库）
│   └── src/
│       ├── main.rs                # 入口：zxs serve | init | import | prune | verify | ...
│       ├── domain/                # 强类型 ID、路径校验、错误码（零 I/O）
│       ├── store/                 # layout.rs(唯一路径拼接) + fs_ops.rs + db.rs(唯一 SQL)
│       ├── service/               # 原子流发布、回收还原、离线运维编排
│       └── api/                   # axum HTTP/1.1 路由与鉴权中间件
│
└── zxs-client/                    # 【子项目 2：跨平台客户端套件】
    ├── .cargo/config.toml         # 锁定 ./vendor 离线源与现代 CPU 编译参数
    ├── Cargo.toml                 # 纯同步 Workspace（core + cli；C3 阶段加入 ffi）
    ├── Cargo.lock
    ├── VENDOR.toml
    ├── VENDOR.sha256
    ├── tools/sync_vendor.py       # 独立 vendor 同步与剪裁脚本
    ├── vendor/                    # 客户端第三方源码（零异步运行时、零重型 GUI 包）
    ├── crates/
    │   └── core/                  # 纯同步内核：domain / store(workspace.sqlite) / transport / zip_bundle / service
    └── apps/
        └── cli/                   # 单一客户端程序：命令行子命令 + `zxs-cli ui` 本机回环浏览器界面（内嵌静态 ES2024 页面）
```

### 2.2 精确依赖清单与 `vendor/` 剪裁规范

* **`zxs-server` 依赖**（`Cargo.lock` 记录 127 个第三方包，含传递依赖和目标平台依赖）：`tokio`（`rt-multi-thread, macros, net, fs, io-util, time, signal, sync`）、`tokio-util`（`io`）、`axum`（`http1, json, query, tokio`）、`futures-util`、`base64`、`rusqlite`（`bundled`）、`blake3`（`rayon`）、`rayon`、`argon2`、`subtle`、`zeroize`、`serde`、`serde_json`、`toml`、`thiserror`、`tracing`、`tracing-subscriber`（`fmt, ansi`）、`clap`（`derive, env`）。
* **`zxs-client` 依赖**（纯同步，`Cargo.lock` 记录 91 个第三方包，含传递依赖和目标平台依赖）：`ureq`（关闭默认 TLS，内置线程安全 HTTP/1.1 Keep-Alive 连接池）、`tiny_http`（仅用于 `zxs-cli ui` 监听 `127.0.0.1:0` 本机管理页）、`base64`、`rusqlite`（`bundled`）、`blake3`（`rayon`）、`getrandom`、`zip` 8.6（关闭默认特性，启用 `deflate-flate2-zlib-rs`）、`ignore`、`rayon`、`serde`、`serde_json`、`thiserror`、`clap`（`derive, env`）。
* **`sync_vendor.py` 剪裁规则**：自动删除各包内 `tests/`、`benches/`、`examples/`、`docs/`、`sqlcipher/` 及所有 32 位、Intel Mac、`gnu`/`gnullvm`、BSD/WASI 非目标平台文件；重写 `.cargo-checksum.json` 为 `{"package": "<upstream_sha256>", "files": {}}` 并输出 `VENDOR.sha256`。构建全程强制 `cargo test --offline --locked`。

---

## 三、服务端存储设计 (`zxs-server`，现代 Linux 专属)

### 3.1 统一标识符、路径与局域网监听护栏

1. **ID 格式**：`container_id` / `op_id` / `recycle_id` 为 **32 位小写十六进制字符串**（128-bit，`container_id` 用 UUIDv7 编码）；`session_token` 为 **64 位小写十六进制字符串**（256-bit 随机数）；`version` 为单调递增 `u32`，目录名 `v000001`。
2. **路径与原件命名规则**：
   * `folder` 以 `/` 开头、不以 `/` 结尾（根为 `"/"`），单段名与 `name` 长度为 `1..=255` UTF-8 字节，拒绝 `/`、`\`、`\0`、控制字符及 `.` / `..`；
   * 同一 `folder` 下 `name` 按小写规范键唯一（保留原始显示大小写，防 Windows/macOS 客户端签出冲突）；
   * 原件命名：若 `name` 含 ASCII 小写字母数字后缀（`1..=10` 字符）则命名为 `payload.<ext>`，否则为 `payload.bin`。
3. **局域网明文护栏**：默认仅允许绑定/连接回环地址、RFC1918 私有局域网段、IPv6 局域网及组网网段（`100.64.0.0/10`）；公网 IP 须显式加 `--allow-public-http`。

### 3.2 极简双目录物理布局

`--data-dir` 与 `--recycle-dir` 可位于同一文件系统的不同目录，或推荐挂载在两块独立 NVMe 分区/逻辑卷上：

```text
<DATA_DIR>/
├── .system/
│   ├── config.toml                    # 口令哈希、监听地址、recycle_dir 路径绑定
│   ├── .lock                          # File::try_lock() 进程排他锁
│   ├── inflight/<container_id>        # 发布改名在途标记（0字节文件，正常为空）
│   └── index.sqlite                   # SQLite WAL 加速索引（删掉可秒级从裸盘重建）
└── containers/<c0..c1>/<container_id>/
    ├── container.json                 # 当前最新元数据（容器写锁内 .tmp + rename 原子替换）
    └── versions/
        ├── .staging-<op_id>/          # 正在流式接收写入的临时目录（StagingGuard 持有，断连自动删除）
        ├── v000001/
        │   ├── version.json           # 一旦发布永不修改
        │   └── payload.mp4            # 独立完整原件（拔盘直接双击播放/打开）
        └── v000002/
            ├── version.json
            └── payload.mp4

<RECYCLE_DIR>/
├── incoming/<recycle_id>/             # 移入回收站中转目录（启动自动清扫中断残留）
└── containers/<c0..c1>/<recycle_id>/
    ├── recycle.json                   # recycle_id, op_id, recycled_at
    └── container/                     # 完整移入的原 <container_id> 目录
```

* **`container.json`**：`{ "container_id": "...", "folder": "/文档", "name": "方案.docx", "tags": ["工作"], "latest_version": 2, "created_at": 1763942400, "updated_at": 1763946000 }`
* **`version.json`**：`{ "version": 2, "op_id": "...", "created_at": 1763946000, "name_at_commit": "方案.docx", "size": 1048576, "blake3": "...", "payload_file": "payload.docx" }`

### 3.3 SQLite 索引与启动自愈

* **`index.sqlite`（`WAL + synchronous=FULL`，1 写 + 8 读连接池）**：维护 `containers`、`versions`、`tags`、`recycle_bin`、`operations`、`sessions`（存 `token_blake3` 与 `expires_at`，服务端重启不掉登录）六张 `STRICT` 表。
* **单步原子流写路径**：
  1. **收流前预检**：先解析请求头核对 `Zxs-Op-Id` 幂等、路径重名与 `expected_version`（冲突在读取正文前 `0.2ms` 立即返回 `409`，绝不白传字节）；
  2. **无锁流式落盘**：在 `versions/.staging-<op_id>/`（由 `StagingGuard` 持有）以 **4–8 MiB 缓冲块**接收网络流写入 `payload.<ext>` 并同步推进 SIMD BLAKE3（若中途断网或空间满报错退出，`StagingGuard::drop` 自动删除 `.staging-<op_id>`），流结束核对声明长度与总 BLAKE3 后调用 `sync_all()`；
  3. **加锁毫秒发布**：持单容器内存锁二次确认版本未变 → 写 `.system/inflight/<cid>` → `std::fs::rename` 将暂存目录发布为 `v00000N` → 原子更新 `container.json` → 提交 SQLite 事务 → 删除 `inflight/<cid>`。
* **启动微秒级自愈**：启动拿 `.lock` 后清扫 `<RECYCLE_DIR>/incoming/` → 遍历 `.system/inflight/<cid>` 清理残余 `.staging-*`/`.tmp` 并按磁盘实存 `container.json` + `versions/v*` 补齐 SQLite 事务 → 删除 `inflight/<cid>`。依托 Linux 文件系统语义，正在流播的视频即便容器被删除移入回收盘，内核也会在流关闭后自动释放原 inode，无需应用层维护读引用计数。

---

## 四、唯一通信契约：8 组极简 HTTP/1.1 接口

所有写请求必带幂等头 `Zxs-Op-Id: <32-hex>`（同号同请求摘要重放返回原结果，同号异参数返 `422`）。鉴权统一支持 `Authorization: Bearer <token>`（及播放器直连 `?token=<token>`）。流式上传接口通过 `Zxs-Params: <Base64URL(UTF-8 JSON)>`、`Zxs-Blake3: <64-hex>` 与 `Content-Length: <u64>` 传递控制参数，请求体直接为纯二进制流 `application/octet-stream`（回滚旧版时 `Content-Length: 0`）。

| # | 方法与路径 | 核心语义 |
| :-: | :--- | :--- |
| **1** | `POST /sessions`<br>`DELETE /sessions/current` | 校验 Argon2id 口令签发 30 天 Bearer Token（持久化哈希至 `sessions` 表）；主动登出。 |
| **2** | `POST /containers` | `Zxs-Params` 传 `{ folder, name, tags }`，请求体直传文件流，一步原子发布首版 `v1`（同名在收流前立返 `409`）。 |
| **3** | `POST /containers/{id}/versions` | `Zxs-Params` 传 `{ expected_version, revert_to_version? }`，请求体直传新版文件流（或回滚空体），原子发布 `latest_version + 1`（版本不匹配在收流前立返 `409`）。 |
| **4** | `PATCH /containers/{id}` | JSON 请求体 `{ folder?, name?, tags? }`，修改元数据并原子重写 `container.json`。 |
| **5** | `GET /containers`<br>`GET /containers/{id}` | 按 `?folder=&tag=&q=&cursor=&limit=` 分页检索（含直接子目录汇总）；查看容器详情与现存升序 `versions` 清单。 |
| **6** | `GET /ops/{op_id}` | 断线后查询任意写操作的精确幂等回执（`200` 已生效 / `404` 未生效）。 |
| **7** | `GET` & `HEAD` `/containers/{id}/versions/{v}/payload` | 4 MiB 大缓冲流式全量下载与标准单区间 HTTP `Range` 读取（供 4K/8K 视频秒开拖动，附强 `ETag: "<blake3>"`）。 |
| **8** | `DELETE /containers/{id}`<br>`GET /recycle`<br>`POST /recycle/{recycle_id}/restore` | 移入 `<RECYCLE_DIR>`；分页查看回收站；指定 `{ folder?, name? }` 还原回 `<DATA_DIR>`（重名返 `409`）。 |

---

## 五、客户端与运维子命令设计

### 5.1 纯同步客户端、本机浏览器 UI 与 `.zxsignore` 整包工作区 (`zxs-client`)

* **双交互入口（合一单二进制 `zxs-cli`）**：
  * **命令行模式**：提供完整终端与脚本化操作（`login`, `ls`, `checkout`, `status`, `commit`, `init-bundle`, `recycle`, `restore`）；
  * **本机图形模式（`zxs-cli ui`）**：仅绑定 `127.0.0.1:0` 随机端口（带一次性访问口令）托管内嵌的零依赖原生 ES2024 管理界面并自动打开系统默认浏览器——既省掉 300 个 GUI 依赖包，又能直接调用本机 Rust 进程读写本地工作区、打开本地默认编辑软件并借助浏览器硬解播放 4K/8K 原片。
* **本地工作区（`workspace.sqlite`）**：并行对比 `(size, mtime_ns)` 毫秒级识别改动；遇 `409` 弹窗三选一（直接从本地高速重流追加为 `latest_version + 1` / 另存新文件 / 备份本地后拉取远端最新版）。
* **`.zxsignore` 整包确定性收发**：
  * `init-bundle` 在目录根生成可见可编辑的初始 `.zxsignore` 模板（预填 `.git/`、`target/`、`node_modules/` 等明文规则）；打包器严格只按 `.zxsignore` 过滤并将 `.zxsignore` 打入包根；
  * 条目按 UTF-8 路径字节序排列、固定归零时间戳；`≤ 4 MiB` 条目在 64 MiB 总内存窗口内由 `rayon` 多核并行执行 SIMD `zlib-rs` Level-1 压缩，`> 4 MiB` 条目单路流式直写（超限自动 ZIP64）；
  * 检出或更新时整包高速下载至本地临时文件，若包根含 `.zxsignore` 则按新旧跟踪清单对比解包（**绝不触碰本地被 `.zxsignore` 忽略的文件**，严格拦截 `..` 路径穿越）。

### 5.2 服务端离线运维子命令 (`zxs-server`)

* `zxs serve [--data-dir <path>] [--recycle-dir <path>] [--allow-public-http]`：启动在线高速服务；
* `zxs init --data-dir <path> --recycle-dir <path>` / `zxs reset-password`：初始化存储目录或重置口令；
* `zxs import <src_dir> --folder <target_folder>`：本机 NVMe 多线程高速导入普通文件；
* `zxs prune --keep-last <N> [--container <cid>]`：离线清理旧历史版本释放空间；
* `zxs verify` / `zxs rebuild-index`：多核 BLAKE3 全盘巡检、全盘扫描重建 `index.sqlite`。

---

## 六、代码设计规范与系统化工程准则

1. **项目内四层单向依赖**：严格按 **`接入外壳层 (api/cli/ui) → 用例编排层 (service) → 基础设施层 (store/transport) → 纯领域层 (domain)`** 自上而下调用。物理路径拼装只准写在 `store/layout.rs`，SQL 语句只准写在 `store/db.rs`，`domain` 层零 I/O。
2. **模块与规模硬红线**：统一使用 `foo.rs` + `foo/bar.rs`（**禁止新建 `mod.rs`**）；**单文件 ≤ 350 行**（不含测试），**单函数 ≤ 60 行**，最大缩进 **≤ 3 层**，**函数参数 ≤ 4 个**（超限封装为结构体）。
3. **强类型与枚举状态机**：外部输入在边界第一时间解析为 `ContainerId`、`OpId`、`FolderPath`、`FileName`、`VersionNum`、`Blake3Hash`；状态机一律使用互斥 `enum`（禁止多个 `bool`/`Option` 拼凑）；所有 JSON/TOML 输入结构体强制 `#[serde(deny_unknown_fields)]`。
4. **静态门禁与错误透明**：根 `Cargo.toml` 固化 `unsafe_code = "forbid"`（除 C3 阶段 `crates/ffi` 外 100% 零 `unsafe`），并 `deny` 全部 `unwrap_used`、`expect_used`、`panic`、`todo`、`dbg_macro`；用 `thiserror` 保留完整上下文链，严禁吞掉底层错误。
5. **RAII 守卫、固定锁序与有界内存**：所有临时目录必须由 `StagingGuard` 持有（未调用 `.publish_rename()` 退出时 `Drop` 自动清理）；全局锁序固定为 **`内存单容器锁 → 文件系统写入/rename → SQLite 写连接`**（持数据库锁期间严禁文件流与网络 I/O）；**严禁对用户正文调用 `fs::read`/`read_to_end` 整文件入内存**（固定 `4–8 MiB` 缓冲流），元数据小文件读取强制设 `1 MiB` 上限。
6. **统一词表与跨项目冒烟门禁**：全栈统一使用 `Container`、`Version`、`Folder`、`LocalPath`、`OpId`、`Recycle` 六个核心名词；仓库 CI 包含一条自动同时编译启动 `zxs-server` 与 `zxs-client` 真连对打的端到端冒烟脚本，第一时间拦截协议漂移。

---

## 七、性能目标与实施里程碑

* **预期总代码量与性能**：全系统核心 Rust 代码约 **3,500 – 4,500 行**；单流读写 **1.5 – 4+ GB/s**（多并发跑满物理网卡与 NVMe）；1 GB 代码库多核打包上传 **< 1.5 秒**；50 万容器冷启动 **< 50 ms**、全盘重建索引 **< 3 秒**；进程常驻缓冲内存 **< 128 MiB**。

| 阶段 | 子项目 | 交付内容与准出硬门槛 |
| :--- | :---: | :--- |
| **S1：Linux 服务端定版** | `zxs-server` | 完成 `vendor/` 剪裁脚本、单数据根+单回收根存储内核、SQLite 索引、8 组 HTTP 接口与全部运维子命令；`--offline` 零警告，断电自愈、稀疏 `prune`、回收还原与线速吞吐测试全绿。 |
| **C1：客户端核心与 CLI** | `zxs-client` | 完成独立 `vendor/`、纯同步 `crates/core` 与 `apps/cli`；联调真实 `zxs-server` 跑通单流原子上传、丢包查回执、`409` 冲突三选一、`.zxsignore` 有界多核整包收发。 |
| **C2：本机浏览器图形界面** | `zxs-client` | 在 `apps/cli` 完成 `ui` 子命令（`127.0.0.1` 回环服务 + 内嵌零 npm 原生 ES2024 页面），三大桌面系统浏览器实测文件管理、唤起本地关联软件编辑、显式提交与 4K/8K 原片秒拖播放。 |
| **C3：移动端原生接入** | `zxs-client` | 新建 `crates/ffi`（200 行零依赖 C-ABI）与 iOS 17+ / Android 13+ 薄壳，完成局域网相册高速备份与流播实机验收。 |
