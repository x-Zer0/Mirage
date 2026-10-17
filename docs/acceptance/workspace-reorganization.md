# Workspace 目录与命名整理（2026-10-18）

## 调整范围

- 根目录改为纯 workspace；`crates/mirage-app`、`crates/mirage-ipc`、`crates/mirage-svc` 共用版本、依赖版本、锁文件和 release profile，默认检查全部成员。
- 主程序保持 `Mirage.exe`，辅助程序改为 `mirage-svc.exe`；服务标识 / 显示名为 `MirageSvc`，管道为 `\\.\pipe\mirage-svc`，程序目录为 `%ProgramFiles%\Mirage\mirage-svc`，注册表为 `HKLM\SOFTWARE\Mirage\MirageSvc`。未增加旧名称兼容。
- 主程序图标、Slint 源码和 build.rs 随 package 管理，根 `assets/core/` 继续供开发与打包复用；打包脚本、安装器与文档同步路径和命名。
- 应用消息、线程句柄、actor 生命周期、配置操作、节点操作、连接事务、账号登录/会话/配置关联、订阅下载与热重载按职责分离；UI 回调、窗口生命周期、状态转发与模型更新分离。
- 增加根 README，突出核心特色并预留截图；设计文档更新实际目录。历史里程碑记录只同步命名和定位路径，保留原验收结果。

## 静态验证

- `cargo fmt --all --check`：通过。
- `cargo check --locked --offline -j 1`：通过，确认默认成员覆盖三个 package。
- `cargo clippy --workspace --all-targets --locked --offline -j 1 -- -D warnings`：通过。
- `cargo build --workspace --release --locked --offline -j 1`：通过。
- PowerShell 脚本语法、Slint 导入和图片引用、开发内核路径、主要文档链接、固定内核和 geodata SHA256：通过。
- 迁移文件逐一核对无遗漏；原有应用方法与 39 个 UI 回调注册均保留。
- 没有新增或运行单元测试，没有进行成本统计。

## 打包验证

`pwsh -NoProfile -File scripts/package.ps1 -SkipBuild` 通过，重新生成 `dist/Mirage-0.1.0-windows-x64-setup.exe` 与 `dist/Mirage-0.1.0-windows-x64-portable.zip` 及各自 SHA256。

公共目录 17 个文件的清单哈希通过；便携 ZIP 包含 `Mirage.exe`、`mirage-svc.exe`、核心、说明和许可证，两个程序与最终 release 构建哈希一致，未包含旧辅助程序名。两种分发产物的 SHA256 文件通过核对。cargo-about 对 Slint 自定义许可证给出缺少 crate 内文件的提示；完整 Slint 许可原文与组件清单已按既有分发规则独立随包提供。

本次未执行真实服务安装 / 卸载、UAC、系统代理切换、TUN 路由或真实账号操作。开发环境旧安装和数据由用户自行清理后重新安装。
