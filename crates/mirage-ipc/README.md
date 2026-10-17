# mirage-ipc

Mirage 主程序与 TUN 辅助服务共用的 Rust 库，统一通信协议、运行配置策略及基础工具。由 `mirage-app` 和 `mirage-svc` 依赖，不生成可执行文件。

## 职责

- **通信契约**：定义请求、响应、错误类型和内核状态，提供同步 / Tokio 异步帧读写。
- **配置策略**：解析 YAML 并展开合并键，覆盖应用托管字段、限制监听地址、隔离 provider 路径，按需生成 TUN 配置。
- **共享基础设施**：统一服务标识、官方 Mihomo SHA256、Windows 标准目录、原子写入和滚动日志。

主程序合成运行配置时调用 `policy::apply`，服务收到配置后再次调用同一实现，确保高权限进程自行执行配置约束。本 crate 不负责界面、订阅下载或服务生命周期，也不依赖另外两个 crate。

## 源码入口

| 文件 | 用途 |
| --- | --- |
| [src/lib.rs](src/lib.rs) | 模块声明与常用类型导出 |
| [src/protocol.rs](src/protocol.rs) | IPC 消息、版本、帧大小限制与编解码 |
| [src/policy.rs](src/policy.rs) | `ManagedOverlay`、`TunOverlay` 与统一配置策略 |
| [src/service.rs](src/service.rs) | `MirageSvc`、`mirage-svc.exe` 和安装记录注册表键 |
| [src/binary.rs](src/binary.rs) | 固定官方内核 SHA256 与文件哈希计算 |
| [src/atomic.rs](src/atomic.rs) / [src/known_folder.rs](src/known_folder.rs) | Windows 原子文件替换与标准目录解析 |
| [src/rolling.rs](src/rolling.rs) | 按日期与大小分段、清理过期日志 |

`atomic` 和 `known_folder` 仅在 Windows 下导出。

## 通信约定

- 管道名称：`\\.\pipe\mirage-svc`；管道连接、鉴权由两端各自实现。
- 帧格式：**4 字节小端 JSON 正文长度 + UTF-8 JSON 正文**，正文上限为 **32 MiB**。
- 首帧为 `Hello`，交换协议版本与程序版本；随后支持 `StartCore`、`ReloadCore`、`StopCore`、`Status` 和 `Cleanup`。
- 启动与重载传递 YAML 内容，不接受调用方指定的可执行文件或配置文件路径。

修改消息结构或语义时同步更新两端；不兼容修改需递增 `PROTOCOL_VERSION`。修改配置策略时递增 `OVERLAY_VERSION`，用于标识配置修订所采用的策略版本。常量均以源码为准。

## 开发

工具链要求见[项目 README](../../README.md#开发)。在仓库根目录执行：

```powershell
cargo check -p mirage-ipc --locked
cargo clippy -p mirage-ipc --all-targets --locked -- -D warnings
```

依赖版本由根目录 `Cargo.toml` 统一管理。进一步说明见[架构设计](../../docs/DESIGN.md)和[服务模块](../mirage-svc/README.md)。
