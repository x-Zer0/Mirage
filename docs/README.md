# Mirage 开发文档

Mirage（暂定名）是 Rust + Slint 编写的 Windows 通用代理客户端：标准官方 Mihomo 内核，普通订阅可直接使用，自由猫（ziyoumao）作为独立账号来源模块接入，后续可扩展 sing-box。

项目介绍与快速开始见 [根 README](../README.md)。本目录保留详细设计、服务商接入说明与验收记录。

| 文档 | 内容 | 何时阅读 |
| --- | --- | --- |
| [DESIGN.md](DESIGN.md) | 产品范围、默认决策、架构、模块接口、配置管线、Mihomo 运行、Windows 集成、持久化、UI、里程碑与验收 | 开工前通读；每个里程碑开始前重读对应章节 |
| [iteration-2-design.md](iteration-2-design.md) | 第二轮迭代：用户覆写层、Hosts、附加规则、IPv6、统一延迟测速、UWP 回环（M9–M12） | 开发 M9 及之后的里程碑前 |
| [ZIYOUMAO-NODE-ANALYSIS-2026-10-16.md](ZIYOUMAO-NODE-ANALYSIS-2026-10-16.md) | 刷新后节点差异、AES 订阅解密、标准核心联网对照、旧订阅结论修正 | 排查节点超时、修改自由猫订阅实现前优先阅读 |
| [ZIYOUMAO.md](ZIYOUMAO.md) | 自由猫主机发现、登录、令牌隔离、订阅下载、旧配置解码、错误映射、已验证证据 | 实现 `providers/ziyoumao` 时 |

## 文档约定

- “必须 / 不得”是实现约束；“建议 / 默认”是可调整的工程决定，调整时同步修改文档。
- 标注【已验证】的结论来自实测；【推测】【待核验】的内容只能作为实现起点，必须在真实环境中确认后再改为【已验证】。
- 文档、示例和日志不得包含真实账号、密码、会话令牌、完整订阅地址或节点凭据。
- 项目要求：不编写单元测试；代码改动以 `cargo fmt --all --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings` 通过为准，功能以有记录的手工端到端验收为准。

## 当前状态

- 2026-10-22 第二轮迭代 M12 已实现：设置页“UWP 回环豁免”弹层，普通权限读取 AppContainer 列表（已实测），保存时保留其他工具的豁免，拒绝访问时经 UAC 提权写入；M9–M12 统一手工验收待进行，见 [acceptance/M12.md](acceptance/M12.md)。
- 2026-10-22 第二轮迭代 M11 已实现：附加规则标签（可视化 16 种常用类型 + YAML 编辑），插在订阅规则之前，目标不存在时跳过并提示，禁止 MATCH；见 [acceptance/M11.md](acceptance/M11.md)。
- 2026-10-22 第二轮迭代 M10 已实现：侧栏新增“规则”页（附加规则标签留待 M11），Hosts 标签支持 hosts 文件格式、按行报错、保存前内核校验与热重载；见 [acceptance/M10.md](acceptance/M10.md)。
- 2026-10-22 第二轮迭代 M9 已实现：用户覆写层、IPv6 三态设置（默认跟随订阅）、测速去除握手延迟（统一延迟，默认开启），设置修改热重载生效；静态检查通过，手工验收待 M9–M12 完成后统一进行，见 [acceptance/M9.md](acceptance/M9.md)。
- 2026-10-18 日志菜单已替换诊断菜单：连接历史在本次运行内按设置保留 0–10,000 条（默认 1,000），托盘期间持续采集；配置操作历史按设置保留 0–60 天（默认 30）。列表布局与实现边界见 [日志菜单说明](logs-redesign.md)。

- 2026-10-17 账号订阅整合已定稿：移除账号菜单，配置页负责添加/覆盖单账号、原客户端本地导入、右侧账号信息抽屉及确认删除。眼睛手动显隐，抽屉关闭后恢复脱敏；列表刷新与账号信息刷新分别保留。交互以 [DESIGN.md §17.1](DESIGN.md#171-页面) 为准，检查范围见 [整合记录](acceptance/account-profile-integration.md)。
- 里程碑：M0～M7 功能已实现；M8 已完成 Windows 安装向导、安装版 / 便携版同时打包和许可证分发，本机普通用户安装 / 覆盖升级 / 卸载检查通过。各阶段的实测证据及剩余环境验收见 [acceptance/](acceptance/)，M8 范围见 [acceptance/M8.md](acceptance/M8.md)。
- 已验证：自由猫账号密码登录（真实账号，2026-10-16）、会话恢复与订阅下载；`clash-verge` 可取得合法 YAML，但本次节点入口不可达；`freecatencua/v` 的 AES 订阅解密后，官方 Mihomo v1.19.32 四节点测速和 HTTPS 204 成功，详见新增分析文档。
- 未验证：令牌有效期、验证码/设备要求、TUN/UDP/IPv6、原客户端退出后的完整独立运行。

## Windows 安装版与便携版（M8）

执行 `pwsh -File scripts/package.ps1` 一次构建同时生成 `dist/Mirage-0.2.0-windows-x64-setup.exe` 和 `dist/Mirage-0.2.0-windows-x64-portable.zip`，分别附 SHA256 文件。两种包均自带 Mihomo、四份 geodata、`mirage-svc.exe` 和许可材料；安装 / 解压不创建服务，第一次启用 TUN 时单独提示安装并请求 UAC。

便携版完整解压后运行 `Mirage/Mirage.exe`；数据仍在 `%LOCALAPPDATA%\Mirage`，与安装版共用。更新前正常退出；删除前先关闭开机启动、按需卸载 TUN 服务，详见包内 `README-portable.txt`。

构建前置工具、升级 / 卸载、许可证与未签名说明见 [packaging.md](packaging.md)，验收范围见 [acceptance/M8.md](acceptance/M8.md)。

## 开发构建

同时生成主程序和辅助服务：

```powershell
cargo build --workspace --release -j 1
```

`target/release/Mirage.exe` 与 `target/release/mirage-svc.exe` 必须配套放置；release 构建运行时还需同目录的 `core/`（官方内核与四份 geodata）。开发环境在仓库根目录执行 `Copy-Item -Path assets/core -Destination target/release -Recurse -Force` 准备资源；仅 debug 构建会自动回退到 `assets/core/`。在设置页安装 / 更新服务时才请求 UAC；TUN 默认关闭。


## 工程组织

三个 package 位于 `crates/`：`mirage-app`、`mirage-ipc`、`mirage-svc`。根目录仅管理 workspace，版本和依赖集中声明；默认构建与检查覆盖三个成员。主程序仍为 `Mirage.exe`，辅助程序为 `mirage-svc.exe`，Windows 服务名为 `MirageSvc`。目录和模块职责见 [DESIGN.md §4](DESIGN.md#4-代码组织)。

`acceptance/` 为各阶段验收记录，命名与路径已随目录整理更新；历史构建时间、文件大小和结论不代表本次重构的重新验收。

本次目录调整与静态验证见 [workspace 整理记录](acceptance/workspace-reorganization.md)。
