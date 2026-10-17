# Mirage 开发文档

Mirage（暂定名）是 Rust + Slint 编写的 Windows 通用代理客户端：标准官方 Mihomo 内核，普通订阅可直接使用，自由猫（ziyoumao）作为独立账号来源模块接入，后续可扩展 sing-box。

后续开发只依赖本目录文档。逆向分析目录（`ziyoumao-analysis/`、`rea-main/`）仅作为取证存档，非必要不再查阅。

| 文档 | 内容 | 何时阅读 |
| --- | --- | --- |
| [DESIGN.md](DESIGN.md) | 产品范围、默认决策、架构、模块接口、配置管线、Mihomo 运行、Windows 集成、持久化、UI、里程碑与验收 | 开工前通读；每个里程碑开始前重读对应章节 |
| [ZIYOUMAO-NODE-ANALYSIS-2026-10-16.md](ZIYOUMAO-NODE-ANALYSIS-2026-10-16.md) | 刷新后节点差异、AES 订阅解密、标准核心联网对照、旧订阅结论修正 | 排查节点超时、修改自由猫订阅实现前优先阅读 |
| [ZIYOUMAO.md](ZIYOUMAO.md) | 自由猫主机发现、登录、令牌隔离、订阅下载、旧配置解码、错误映射、已验证证据 | 实现 `providers/ziyoumao` 时 |

## 文档约定

- “必须 / 不得”是实现约束；“建议 / 默认”是可调整的工程决定，调整时同步修改文档。
- 标注【已验证】的结论来自实测；【推测】【待核验】的内容只能作为实现起点，必须在真实环境中确认后再改为【已验证】。
- 文档、示例和日志不得包含真实账号、密码、会话令牌、完整订阅地址或节点凭据。
- 项目要求：不编写单元测试；代码改动以 `cargo fmt --check`、`cargo check`、`cargo clippy`、release 构建通过为准，功能以有记录的手工端到端验收为准。

## 当前状态

- 2026-10-17 账号订阅整合已定稿：移除账号菜单，配置页负责添加/覆盖单账号、原客户端本地导入、右侧账号信息抽屉及确认删除。眼睛手动显隐，抽屉关闭后恢复脱敏；列表刷新与账号信息刷新分别保留。交互以 [DESIGN.md §17.1](DESIGN.md#171-页面) 为准，检查范围见 [整合记录](acceptance/account-profile-integration.md)。
- 里程碑：M0～M6 已完成；M7 已补齐代码并完成基础验收，管理员权限下的 TUN 实机验收仍待完成。详见 [acceptance/M7.md](acceptance/M7.md)，其余阶段见 [acceptance/](acceptance/)。
- 已验证：自由猫账号密码登录（真实账号，2026-10-16）、会话恢复与订阅下载；`clash-verge` 可取得合法 YAML，但本次节点入口不可达；`freecatencua/v` 的 AES 订阅解密后，官方 Mihomo v1.19.32 四节点测速和 HTTPS 204 成功，详见新增分析文档。
- 未验证：令牌有效期、验证码/设备要求、TUN/UDP/IPv6、原客户端退出后的完整独立运行。

## M7 构建

同时生成主程序和辅助服务：

```powershell
cargo build --workspace --release -j 1
```

`target/release/Mirage.exe` 与 `target/release/mirage-service.exe` 必须配套放置；分发时还需同目录的 `core/`（官方内核与四份 geodata）。开发环境可复用 `assets/core/`。在设置页安装 / 更新服务时才请求 UAC；TUN 默认关闭。
