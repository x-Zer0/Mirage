# mirage-app

Mirage 的 Windows 桌面主程序，使用 **Rust + Slint** 和软件渲染，生成 **`Mirage.exe`**。负责用户界面、配置与账号管理、代理连接及系统集成，以普通用户权限运行。

## 核心职责

- **配置管理**：订阅链接、本地 YAML 和自由猫账号接入，支持定时更新、配置校验、修订管理、热重载及失败回退。
- **代理连接**：管理官方 Mihomo 内核，提供规则 / 全局 / 直连、节点选择与测速、系统代理和 TUN 切换。
- **桌面交互**：窗口、托盘、单实例、开机启动、实时流量、内核日志与诊断导出。
- **数据与恢复**：保存设置和配置元数据，保护凭据、脱敏日志，在启动及退出时执行所属内核与系统代理的恢复清理。

## 源码入口

| 位置 | 用途 |
| --- | --- |
| [src/main.rs](src/main.rs) | 单实例检查、启动恢复、运行时与 UI 组装、退出清理 |
| [src/app/](src/app/) | 应用状态与业务编排：连接、节点、配置、账号、更新、TUN 和诊断 |
| [src/ui_bridge/](src/ui_bridge/) | Slint 回调注册、状态转发、模型转换及窗口 / 托盘生命周期 |
| [src/domain/](src/domain/) | 数据模型、错误类型、敏感字符串与脱敏规则 |
| [src/subscriptions/](src/subscriptions/) | 下载、格式识别、本地导入、用量解析与配置修订 |
| [src/providers/](src/providers/) | 账号服务商适配，目前实现自由猫接入 |
| [src/cores/mihomo/](src/cores/mihomo/) | 内核校验、配置合成、控制器 API 与普通 / 服务启动器 |
| [src/platform/windows/](src/platform/windows/) | 系统代理、凭据、服务、进程、单实例、自启与 ACL |
| [src/storage/](src/storage/) | 数据目录、元数据、原子写入、恢复记录与 ZIP 导出 |
| [src/net/](src/net/) / [src/logging/](src/logging/) | 公共 HTTP 逻辑与日志初始化 |
| [ui/](ui/) / [assets/icons/](assets/icons/) | Slint 主窗口、页面、共享组件、服务商表单与图标 |
| [build.rs](build.rs) | 编译 `ui/app.slint`，生成并嵌入 Windows 清单和图标 |

业务入口见 `app/messages.rs`、`app/actor.rs` 和 `app/handlers.rs`；界面交互入口见 `ui_bridge/callbacks.rs`，状态呈现见 `ui_bridge/models.rs`。

## 运行与依赖边界

UI 通过 `Command` 消息提交操作，独立运行时线程中的应用 actor 统一编排业务，并通过 `AppStateView` 状态快照更新界面；网络请求和内核管理在后台执行。

普通模式由主程序直接启动 Mihomo；TUN 模式通过命名管道交给 [mirage-svc](../mirage-svc/README.md) 管理高权限内核，服务安装 / 更新单独请求 UAC。主程序通过 [mirage-ipc](../mirage-ipc/README.md) 复用协议、配置策略和基础工具，不以 Rust 依赖方式引用 `mirage-svc`。

## 开发

需要 Rust **1.92+**、Windows MSVC 工具链和 PowerShell 7，完整要求见[项目 README](../../README.md#开发)。在仓库根目录执行：

```powershell
pwsh -File scripts/fetch-core.ps1
cargo build --workspace --locked
cargo run -p mirage-app --bin Mirage --locked
```

首次下载内核资源需要联网。workspace 构建同时生成 `target/debug/Mirage.exe` 和 `target/debug/mirage-svc.exe`，便于调试 TUN；运行 TUN 仍需在界面中完成服务安装。

内核资源优先读取 `Mirage.exe` 同级的 `core/`；该目录没有 `mihomo.exe` 时回退到仓库根目录的 `assets/core/`。Slint 源码和图标位于本 crate，内核与 geodata 由根目录脚本准备。

静态检查：

```powershell
cargo fmt --all --check
cargo check -p mirage-app --locked
cargo clippy -p mirage-app --all-targets --locked -- -D warnings
```

版本与依赖版本由 workspace 统一管理。完整分发流程见[打包文档](../../docs/packaging.md)。

## 数据位置

设置、账号与配置元数据、配置修订、缓存、日志和恢复记录位于 `%LOCALAPPDATA%\Mirage`，安装版与便携版共用该目录。实际位置通过 Windows Known Folder API 解析，凭据使用 Windows Credential Manager / DPAPI 保护，并绑定当前用户。

普通内核工作目录为该目录下的 `core-home/`；TUN 服务使用独立的受保护目录，详见[服务模块文档](../mirage-svc/README.md#文件与安装记录)。更多模块约定见[架构设计](../../docs/DESIGN.md#4-代码组织)。
