# mirage-svc

Mirage 的 Windows TUN 辅助服务，生成 **`mirage-svc.exe`**，注册服务名为 **`MirageSvc`**。以 **LocalSystem** 身份管理启用 TUN 的 Mihomo 内核，让 `Mirage.exe` 保持普通用户权限运行。

服务按需安装，启动类型为手动。首次启用 TUN 时由主程序说明并通过 UAC 调用安装入口；普通代理模式由主程序直接管理内核。

## 核心职责

- **受控通信**：通过 `\\.\pipe\mirage-svc` 接收请求，限制本机指定用户连接，并核对客户端进程映像路径与安装记录。
- **配置复核**：复用 `mirage-ipc` 的配置策略，将配置写入受保护工作目录；启动、重载前校验固定官方内核 SHA256，并执行 `mihomo -t`。
- **进程管理**：启动、重载、停止内核，报告运行状态；使用 Windows Job Object 约束内核生命周期。
- **退出清理**：客户端断开、15 秒内未收到请求或服务停止时终止所属内核；服务启动时核对并清理异常退出记录。

服务只接受配置内容，使用自身受保护目录中的内核与工作文件。订阅、账号、节点选择和系统代理设置由 `mirage-app` 负责。协议与程序版本不匹配时拒绝启动、重载内核。

## 源码入口

| 文件 | 用途 |
| --- | --- |
| [src/main.rs](src/main.rs) | 命令行分派与服务入口 |
| [src/install.rs](src/install.rs) | 安装、更新、卸载及受保护文件复制 |
| [src/service.rs](src/service.rs) | Windows 服务控制管理器（SCM）接入与停止通知 |
| [src/pipe.rs](src/pipe.rs) | 管道鉴权、握手、会话与请求分派 |
| [src/core.rs](src/core.rs) | 配置复核、内核状态与启动 / 重载 / 停止协调 |
| [src/process.rs](src/process.rs) | 内核进程、Job Object、配置检查与日志输出 |
| [src/identity.rs](src/identity.rs) / [src/record.rs](src/record.rs) | 进程身份核对与安装记录 |
| [src/acl.rs](src/acl.rs) / [src/paths.rs](src/paths.rs) / [src/log.rs](src/log.rs) | 访问权限、标准目录与服务日志 |

依赖关系为 `mirage-svc → mirage-ipc`，不依赖主程序或 Slint。

## 安装与运行

日常使用由主程序管理服务；开发排查时可使用以下入口：

| 参数 | 行为 |
| --- | --- |
| `install --client <Mirage.exe绝对路径> --user-sid <用户SID> [--core-source <core目录>]` | 管理员权限安装或更新并启动服务 |
| `uninstall` | 管理员权限停止并删除服务、安装记录、专用程序目录及 TUN 工作目录 |
| 无参数 | 由 SCM 启动服务进程 |

`--user-sid` 可省略，默认取执行安装进程的用户 SID；应与运行主程序的用户一致。`--core-source` 默认取 `Mirage.exe` 同级的 `core/`，需包含 `mihomo.exe`、`GEOIP.metadb`、`GEOSITE.dat`、`GEOIP.dat` 和 `ASN.mmdb`。开发时可显式指定仓库的 `assets/core/`。

服务绑定安装时记录的用户 SID 和 `Mirage.exe` 规范路径；移动主程序后应重新安装服务。无参数入口供 SCM 调用，不作为控制台启动方式。

## 文件与安装记录

| 位置 | 内容 |
| --- | --- |
| `%ProgramFiles%\Mirage\mirage-svc` | 服务、官方内核与 geodata 的受保护副本 |
| `%ProgramData%\Mirage\tun-home` | 运行配置、provider 缓存、进程记录与内核日志 |
| `%ProgramData%\Mirage\logs` | 服务滚动日志，文件名为 `mirage-svc-<日期>[.<分段>].log` |
| `HKLM\SOFTWARE\Mirage\MirageSvc` | 客户端路径、用户 SID、版本与内核 SHA256 |

文件路径通过 Windows Known Folder API 解析。SYSTEM 和管理员可写，安装用户只读；卸载服务不清理主程序的 `%LOCALAPPDATA%\Mirage` 数据。

## 开发

使用 Windows MSVC 工具链，完整要求见[项目 README](../../README.md#开发)。在仓库根目录执行：

```powershell
cargo check -p mirage-svc --locked
cargo clippy -p mirage-svc --all-targets --locked -- -D warnings
cargo build -p mirage-svc --release --locked
```

产物为 `target/release/mirage-svc.exe`。编译不会注册或启动服务。协议细节见[mirage-ipc](../mirage-ipc/README.md)，完整分发流程见[打包文档](../../docs/packaging.md)。
