# Mirage

基于 **Rust + Slint** 的 Windows 桌面代理客户端，使用官方 **Mihomo** 内核。支持 Windows 10 1809+ / Windows 11 x64，目前处于开发阶段。

## 核心特色

- **原生桌面界面**：Slint 软件渲染，无需 WebView；支持托盘、开机启动、实时流量和内核日志。
- **统一配置管理**：订阅链接、本地 YAML、自由猫账号接入；支持定时更新、配置校验、热重载及失败回退。
- **灵活连接**：系统代理与 TUN 模式，支持规则 / 全局 / 直连、节点搜索、测速和选择记忆。
- **权限隔离**：主程序以普通用户运行，TUN 通过独立辅助服务管理高权限内核；首次安装服务单独请求 UAC。
- **隐私与恢复**：凭据使用 Windows Credential Manager / DPAPI 保存，日志脱敏，异常退出后恢复本应用设置的系统代理并清理所属内核。
- **两种分发方式**：安装版与免安装 ZIP，随包提供内核、geodata 和第三方许可材料。

<!-- 截图预留：补充 docs/screenshots/overview.png 后启用下面的图片。 -->
<!-- ![Mirage 总览](docs/screenshots/overview.png) -->

## 使用

安装后或完整解压便携包，运行 `Mirage.exe`，在「配置」页添加订阅、导入 YAML 或登录自由猫账号，选择配置后连接。按需开启系统代理或 TUN；TUN 默认关闭。

便携版也将设置、订阅和日志保存到 `%LOCALAPPDATA%\Mirage`，与安装版共用。凭据绑定当前 Windows 用户，不随程序目录迁移。环境验收进度见 [开发文档](docs/README.md)。

## 开发

需要 Rust **1.92+**（`x86_64-pc-windows-msvc`）、MSVC C++ 构建工具和 PowerShell 7。在仓库根目录执行：

```powershell
pwsh -File scripts/fetch-core.ps1
cargo build --locked
cargo run -p mirage-app --bin Mirage --locked
```

首次下载需要联网。`cargo build` 默认构建全部三个 package，产物为 `target/debug/Mirage.exe`、`target/debug/mirage-svc.exe`；开发运行使用根目录 `assets/core/` 的内核资源。

静态检查覆盖整个 workspace，不编写或运行单元测试：

```powershell
cargo fmt --all --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

## 工程结构

```text
crates/
  mirage-app/       主程序：Rust 业务、ui/ 界面、assets/ 图标、build.rs
  mirage-ipc/       共享协议、配置策略、内核校验与基础设施
  mirage-svc/       TUN 辅助服务，生成 mirage-svc.exe
assets/core/       下载的内核与 geodata（不入库）
scripts/           资源准备与打包脚本
packaging/         安装器、分发说明与第三方许可材料
docs/              设计、开发说明与验收记录
```

版本、依赖版本和 release profile 统一在根目录 `Cargo.toml` 管理，成员按需开启依赖特性。模块职责和依赖约定见 [架构设计](docs/DESIGN.md#4-代码组织)。

## 打包

准备 Inno Setup 6.4.3 和 cargo-about 0.9.2 后执行：

```powershell
pwsh -File scripts/package.ps1
```

一次生成 `dist/` 下的安装版、便携 ZIP 及 SHA256 校验文件。工具配置和分发说明见 [打包文档](docs/packaging.md)；第三方许可见 [THIRD-PARTY-NOTICES](packaging/THIRD-PARTY-NOTICES.txt)。
