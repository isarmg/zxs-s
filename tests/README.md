# ZXS 行为测试

从仓库根目录运行 `python3 tests/run.py`。Linux 默认依次运行 Rust、系统集成、原生桌面和 Chromium 测试。macOS 和 Windows 默认运行 Rust 和原生桌面测试。测试使用自动分配的回环端口、临时工作区、临时配置和专用数据盘。

## 运行方式

| 要验证的范围 | 命令 |
| --- | --- |
| Vendor 清单、格式、严格 Clippy、Rust 单元测试、调试构建 | `python3 tests/run.py --suite rust` |
| Linux 服务端、CLI、移动 C ABI 和数据完整性 | `python3 tests/run.py --suite integration` |
| Linux 跨文件系统回收和还原 | `python3 tests/run.py --suite integration --recycle-parent /dev/shm` |
| 原生桌面历史恢复、跨盘路径和工作区选择 | `python3 tests/run.py --suite desktop` |
| Chromium 中实际点击和文件保存 | `python3 tests/run.py --suite browser` |
| Android APK 的界面流程 | `python3 tests/run.py --suite android --android-apk /path/to/app-debug.apk` |

可重复传入 `--suite`。Python 需要 3.11 或更新版本。集成与浏览器测试使用 `rust` 生成的调试二进制。外部构建可通过 `ZXS_SERVER_BIN`、`ZXS_CLIENT_BIN` 和 `ZXS_FFI_LIB` 指定绝对路径。跨文件系统用例检查回收根与临时工作区分属不同文件系统。

Chromium 需要 Node.js 22 及 Playwright。使用项目锁定的测试依赖，也可用 `ZXS_CHROMIUM_EXECUTABLE` 指定已安装的 Chromium。

```sh
npm --prefix tests ci --ignore-scripts
cd tests
npx --no-install playwright install --with-deps --only-shell chromium
cd ..
python3 tests/run.py --suite browser
```

Android 测试需要已启动、已解锁的专用测试设备及 `adb`。测试在设备上安装 APK、连接临时服务器并驱动界面。Android 模拟器使用 `gradle -PzxsEmulator :app:assembleDebug` 生成的 x86_64 APK。macOS 的原生桌面测试通过 `xcrun swiftc` 编译 Swift 桥接测试，读取 `cargo build --workspace` 生成的 C ABI 动态库。CI 另用 Xcode 构建 iOS 应用和 File Provider，并在模拟器安装与启动应用。

## 覆盖与断言

| 行为 | 覆盖内容 | 用例 |
| --- | --- | --- |
| 磁盘登记和离线运维 | 初始化、非空目录拒绝、补登记、盘标记、在线独占锁、导入、巡检、清理保留版本与索引重建 | `smoke.py`、`regressions.py`、`test_server_behavior.py` |
| HTTP 与会话 | 登录、撤销、服务重启、口令变更、未授权的原件及写入、HEAD、完整与区间正文、强 ETag、零字节和无效范围 | `smoke.py`、`regressions.py`、`test_server_behavior.py` |
| 目录、检索与元数据 | 大小写及 NFC、空目录、路径层级、重命名、标签筛选、跨盘查询、逐页结果、批量 ID 和游标边界 | `smoke.py`、`regressions.py`、`test_server_behavior.py`、`browser.mjs` |
| 追加及数据完整性 | 同基线的并发写入只发布一版、上传校验、中断清理、重试认领、损坏与缺版、读取 `/payload` 字节及实际磁盘文件 | `smoke.py`、`regressions.py`、`test_server_behavior.py`、Rust 单元测试 |
| 移动、删除和回收 | 跨盘冻结相交写入、同盘重命名、递归回收、还原与彻底删除、跨文件系统复制、断电事务恢复 | `smoke.py`、`regressions.py`、Rust 恢复用例 |
| 桌面工作区 | 本地新增、空目录、显式目录提交、切换工作区、跨盘路径提示、历史恢复、备份冲突、原件路径占用、干净文件同步、下载失败后保留旧文件并重试 | `test_workspace_behavior.py`、`platform_history.py`、`workspace_selection.py`、`browser.mjs` |
| 文件传输 | 限速上传和下载的中间进度和速率、终端 stdout JSON、浏览器原件文件名和实际字节、零正文和缓存命中回调 | `test_transfer_behavior.py`、`test_mobile_behavior.py`、`smoke.py`、`browser.mjs`、`apple_bridge.swift` |
| ZIP 工作区 | 明文标记、确定性打包、目录成员分页、忽略项、本地解包、整包缓存、历史版本类型切换与离线浏览 | `smoke.py`、`regressions.py`、`browser.mjs`、`android_smoke.py`、Rust 单元测试 |
| 移动上传与预览 | 普通文件和版本提交、冲突、当前目录照片批次、同名认领及另存、原件缓存、媒体 Range、Swift C ABI 下载、Android 盘内上传入口 | `test_mobile_behavior.py`、`smoke.py`、`apple_bridge.swift`、`android_smoke.py` |

## 定位失败

Linux 的行为用例使用 `unittest` 输出单项名称和表格用例参数。可按模块、类或单项运行。运行前按上述方式构建调试二进制。

```sh
python3 -m unittest discover -s tests -p 'test_server_behavior.py' -v
PYTHONPATH=tests python3 -m unittest test_workspace_behavior.WorkspaceBehavior.test_moved_destination_preserves_untracked_occupant_and_custom_checkout -v
node --test --test-name-pattern='file transfers' tests/browser.mjs
```

涉及故障的用例同时检查操作结果、可见版本和保存字节。例如并发追加验证赢家的正文和连续版本号，断线上传检查暂存清理后可直接重试，跨盘恢复核对原改动的备份、目标文件及远端版本。性能用例以受控限速获得中间样本，速率不依赖运行机器的网卡额定带宽。

iOS 模拟器任务验证应用与扩展的集成构建和启动。系统相册选择器、系统「文件」的进度呈现、真实 NVMe 与 10GbE 吞吐需要相应设备验收。Android 的模拟器脚本覆盖应用内入口与离线续接，当前目录上传的数据结果由共享 C ABI 用例核对。
