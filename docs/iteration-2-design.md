# 第二轮迭代设计：附加规则、Hosts、IPv6、测速口径、UWP 回环

日期：2026-10-22。状态：需求已确认，待开发。本文是开新会话开发时的唯一依据；实现过程中如果调整决策，需要同步修改本文和 [DESIGN.md](DESIGN.md) 中的对应章节。

## 0. 已确认的决策

| # | 需求 | 结论 |
| --- | --- | --- |
| 1 | 追加 hosts | 全局一份，写进 Mihomo 运行配置的 `hosts`，不修改系统 hosts 文件；只对经过内核的流量生效 |
| 2 | UWP 回环 | Windows 没有图形界面，只有命令行工具 `CheckNetIsolation.exe`。Clash 系客户端打包的是 Telerik 的闭源工具 `EnableLoopback.exe`，不采用。改为自己实现，工作量评估为中低（§6.6），排在最后一个里程碑，必要时可以单独砍掉 |
| 3 | 附加规则 | 全局一份，插在订阅规则之前；提供可视化编辑和 YAML 编辑两种模式；指向的策略在当前配置中不存在时，跳过该条并提示，不让配置失败 |
| 4 | IPv6 | 目前没有支持：应用不写 `ipv6`，实际行为取决于订阅；TUN 注入的最小 DNS 不返回 AAAA。新增三态设置，默认“跟随订阅”，与现有行为一致 |
| 5 | 测速口径 | 新增设置“测速去除握手延迟”，默认开启，开启时强制 `unified-delay: true`；关闭时跟随订阅 |
| — | 入口 | 侧栏新增“规则”页，分为“附加规则”和“Hosts”两个标签；IPv6 和测速开关放在设置页；UWP 入口放在设置页的“网络”分区 |

## 1. 总体方案：用户覆写层

需求 1、3、4、5 都要修改运行配置，统一用一个新的“用户覆写层”来实现，**放在安全策略之后执行**：

```
订阅原文 ─detect→ policy::apply（安全策略，mirage-ipc）─→ 摘要比对 ─→ 用户覆写层（新增，仅 app 侧）─→ run/config.yaml
```

- 把用户覆写层放在安全策略之后有三个原因：
  1. `policy::ensure_dns` 会在 TUN 模式下整体替换 `dns` 块，覆写层排在后面，`dns.ipv6` 和 `dns.use-hosts` 才不会丢失。
  2. 现有摘要比对（`compose.rs`：合成前后摘要必须一致）可以保持不变，追加的规则不会触发“合成器缺陷”错误。
  3. 覆写层只修改下面白名单中的键，不涉及任何安全相关的键。
- TUN 服务收到 YAML 后还会再执行一次 `policy::apply`。该函数不会删除或覆盖白名单中的键；此时 `dns.enable` 已经为 true，`ensure_dns` 不会再次替换 DNS。因此 **mirage-ipc 和 mirage-svc 都不需要改动，`OVERLAY_VERSION` 也不用加 1**，服务也不必升级。
- 覆写层允许修改的键（白名单）：`rules`（只在前面插入）、`hosts`（合并）、`dns.use-hosts`、`ipv6`、`dns.ipv6`、`unified-delay`、`find-process-mode`（只从 `off` 提升为 `strict`）。
- 版本摘要（`ConfigRevision.summary`）仍然记录订阅本身的数量，不把用户规则算进去。

### 1.1 代码落点

- `cores/mihomo/compose.rs`：新增 `UserOverlay` 结构，在 `ManagedOverlay` 中增加 `user: &UserOverlay` 字段。`compose()` 在摘要比对通过后，重新解析 `policy::apply` 输出的 YAML，应用用户覆写，再序列化。`Composed.policy_notes` 中追加覆写说明（例如“跳过 2 条附加规则：策略组不存在”）。
- 所有构造 `ManagedOverlay` 的地方都要显式传入 `UserOverlay`。【M9 定稿】`app/pipeline.rs`（导入、更新校验）传空覆写 `UserOverlay::default()`，只判定订阅本身，避免用户覆写导致的 `-t` 失败把订阅版本标为无效；`app/runtime.rs` 的启动、热重载和覆写重载传当前设置生成的覆写。
- `UserOverlay` 由 `AppCore::user_overlay()`（`app/overlay.rs`）根据 settings（M10/M11 起加上覆写文件）生成，作为一份只读快照传给后台任务；`Running.user` 记录运行中配置实际使用的覆写。
- 【M9】`UserOverlay` 先只有 `ipv6` 和 `unified_delay` 两个字段，`rules`/`hosts`、`custom_*_enabled` 和 `overrides/` 目录随 M10/M11 加入，不提前留空字段。

```rust
pub struct UserOverlay {
    pub rules: Vec<String>,          // 已校验的 Mihomo 规则字符串，空表示不追加
    pub hosts: Vec<(String, Vec<IpAddr>)>, // 已校验的域名与地址
    pub ipv6: Ipv6Mode,              // Follow | On | Off
    pub unified_delay: bool,         // true 时强制 unified-delay: true
}
```

### 1.2 持久化

- `settings.json`（`storage/meta.rs::SettingsFile`）中新增以下字段，都要带 `#[serde(default)]`，兼容旧文件：
  - `custom_rules_enabled: bool`，默认 true
  - `custom_hosts_enabled: bool`，默认 true
  - `ipv6: Ipv6Mode`，默认 `Follow`，序列化为 `"follow" | "on" | "off"`
  - `unified_delay: bool`，默认 true（老用户升级后测速口径会自动切换，属于预期行为）
- 新建目录 `%LOCALAPPDATA%\Mirage\overrides\`，在 `storage/layout.rs` 中增加 `overrides_dir()`：
  - `rules.yaml`：内容为 `rules:` 序列，格式与 Mihomo 相同，是附加规则的唯一数据源
  - `hosts.txt`：hosts 文件格式的原文，保留用户注释
  - 两个文件都用 `storage::atomic` 原子写入；文件不存在时视为空

### 1.3 生效流程（规则页保存、相关设置变化时共用）

【M9 实现】设置页保存先写 settings，再由 `sync_user_overlay` 对运行中的当前版本执行 `runtime::reapply`：合成 + `-t` → 热重载 → 失败时用原覆写重载回去（`OverlayOutcome::Rejected / RolledBack / NeedsRestart`）。失败时把 IPv6 / 统一延迟设置恢复为原值并保存，操作日志记“应用设置 / 失败”。启动或订阅热重载进行中时不立即执行，等其结束（`commit_queued_revision`）后比较 `Running.user` 与设置再补做。M10/M11 的规则页保存需要“先校验再写文件”，届时按下列步骤实现。

1. 先做语法校验（§3.4、§2.3）。不通过就逐行提示错误，不写入文件。
2. 如果当前有活动配置，用新的 `UserOverlay` 合成该配置当前的版本，再执行 `check::check`（即 `mihomo -t`）。不通过就展示内核报错，不写入文件。
3. 原子写入文件和 settings。
4. 如果内核正在运行，复用 `app/reload.rs` 的热重载流程，对当前版本执行一次重载，失败时回滚。端口和 TUN 都没有变化，不需要重启进程。
5. 写一条操作日志，例如“保存附加规则 / 成功 / 12 条”。

其他配置只会在被切换到时用新的覆写重新合成。如果此时内核校验失败，错误提示中注明“可能由附加规则或 Hosts 导致”，并引导用户到规则页。

## 2. Hosts（需求 1）

### 2.1 语义

- 写入运行配置的顶层 `hosts`。合并规则：先保留订阅自带的 hosts，同一个键以用户填写的为准。
- 用户 hosts 非空且已启用时，如果运行配置中存在 `dns` 块，强制设置 `dns.use-hosts: true`，保证 TUN 和 fake-ip 下 DNS 应答也会使用这些映射。
- 生效范围：
  - 直连（DIRECT）连接：生效，内核会用 hosts 解析。
  - TUN 模式下的 DNS 应答：生效。
  - 经代理节点转发的连接：内核通常会把域名原样发给远端节点，由远端解析，因此 hosts 映射**可能不生效**。这一点【待核验】，必须在 M10 中实测。如果确认不生效，页面说明文字要写明“对直连流量和 TUN 下的 DNS 解析生效”。
- 不经过 Mirage 的流量（没有走系统代理的程序）完全不受影响，这也符合需求。

### 2.2 输入格式（hosts 文件风格）

```
# 注释
192.168.1.10   nas.local
10.0.0.1       *.dev.example.com   api.example.com
::1            v6.local
```

- 每行第一列是 IP（IPv4 或 IPv6），后面跟一个或多个域名。
- 域名支持精确域名、`*.example.com`（通配一级子域）和 `+.example.com`（匹配域名本身及所有子域），与 Mihomo 一致。
- 同一个域名出现在多行时，合并为 IP 列表（Mihomo 支持一个域名对应多个 IP）。
- 本轮不支持“域名 → 域名”的别名映射。

### 2.3 校验

- IP 用 `std::net::IpAddr` 解析。域名只允许 `[A-Za-z0-9-.*+_]`，`*`/`+` 只能出现在开头，单个标签不超过 63 个字符，总长不超过 253 个字符。
- 上限：最多 2000 行，单行最长 1024 个字符；超出上限视为错误。
- 错误按行号汇总展示，例如“第 3 行：IP 地址不合法”。

## 3. 附加规则（需求 3）

### 3.1 语义

- 规则插在订阅 `rules` 的**最前面**，因此优先级高于订阅规则。订阅没有 `rules` 时，直接新建这个序列。
- 只在“规则”路由模式下生效；全局或直连模式下，页面顶部给出提示。
- 规则中的目标策略在运行配置中不存在时，**跳过该条**并在提示中列出，不影响其他规则。可用的目标包括：
  - 内置策略 `DIRECT`、`REJECT`、`REJECT-DROP`、`PASS`
  - `proxy-groups` 中的组名
  - `proxies` 中的节点名
- `RULE-SET` 引用的 provider 在当前配置中不存在时，同样跳过。
- 禁止写 `MATCH`（它会让所有订阅规则失效），校验时直接报错。
- 规则中包含 `PROCESS-NAME`/`PROCESS-PATH` 而订阅设置了 `find-process-mode: off` 时，改为 `strict`，并在提示中说明。

### 3.2 可视化编辑器支持的规则类型

| 分类 | 类型 | 输入示例 | 附加选项 |
| --- | --- | --- | --- |
| 域名 | `DOMAIN` | `www.example.com` | |
| | `DOMAIN-SUFFIX` | `example.com` | |
| | `DOMAIN-KEYWORD` | `google` | |
| | `DOMAIN-WILDCARD` | `*.example.*` | |
| | `DOMAIN-REGEX` | `^ads?\.` | |
| | `GEOSITE` | `cn`、`category-ads-all` | |
| IP | `IP-CIDR` | `1.2.3.0/24`；输入 IPv6 时自动改用 `IP-CIDR6` | `no-resolve` |
| | `IP-CIDR6` | `2001:db8::/32` | `no-resolve` |
| | `IP-ASN` | `13335` | `no-resolve` |
| | `GEOIP` | `CN`、`private` | `no-resolve` |
| | `SRC-IP-CIDR` | `192.168.1.0/24` | |
| 端口 | `DST-PORT` | `443`、`8000-9000`、`80/443` | |
| | `SRC-PORT` | 同上 | |
| 进程 | `PROCESS-NAME` | `chrome.exe` | |
| | `PROCESS-PATH` | `C:\Program Files\App\app.exe` | |
| 网络 | `NETWORK` | `tcp` / `udp` | |

- 以下写法只能在 YAML 模式中编辑：`AND`/`OR`/`NOT` 逻辑规则、`RULE-SET`、`SUB-RULE`、`IN-*`、`DSCP`、`UID` 以及其他 Mihomo 支持的规则。这些规则在可视化列表中显示为“高级规则”，只读，但可以排序和删除。
- 输入 IP 时只填单个地址（如 `1.2.3.4`），自动补全为 `/32` 或 `/128`。

### 3.3 规则页 UI（附加规则标签）

- 顶部依次是：“启用附加规则”开关、“可视化 / YAML”模式切换、条数统计、非规则模式时的提示。
- 可视化模式下，列表每行显示：序号、类型、匹配内容、目标策略、上移、下移、编辑、删除。条数多时复用节点页的虚拟滚动或分页方式。
- 点击“添加规则”或“编辑”打开弹层（复用 `components/overlay.slint`），包含以下控件：
  - 类型下拉框，按分类分组
  - 匹配内容输入框，按类型显示示例占位文字
  - `no-resolve` 勾选框，只对 IP 类规则显示
  - 目标下拉框，选项依次为内置策略、当前运行配置的策略组名（来自 `view.groups`），并允许手动输入
  - 弹层内即时校验，不通过时禁用“确定”按钮
- YAML 模式下是多行文本框，内容就是 `rules.yaml` 的原文，示例如下：

  ```yaml
  rules:
    - DOMAIN-SUFFIX,example.com,DIRECT
    - IP-CIDR,10.0.0.0/8,DIRECT,no-resolve
    - AND,((DOMAIN-SUFFIX,example.org),(NETWORK,udp)),REJECT
  ```

- 模式切换：
  - 可视化 → YAML：把当前列表序列化为文本。
  - YAML → 可视化：先解析文本；失败时不允许切换，并展示错误。
  - 从可视化保存会重新生成 YAML，YAML 中的注释会丢失；切换时给出一次提示。
- 底部是“保存”和“放弃修改”按钮，交互与设置页一致；保存后按 §1.3 生效。

### 3.4 校验

- 应用侧做结构校验：
  - 每条规则是字符串，用逗号分隔后不少于 3 段（`类型,内容,目标[,参数]`）。逻辑规则的括号需要配平。
  - 类型必须在 Mihomo 已知的规则类型集合内。
  - 对已知类型做内容格式检查：CIDR 用 `ipnet` 风格解析，端口在 1–65535 之间且区间合法，ASN 是数字，`NETWORK` 只能是 tcp 或 udp。
  - 禁止 `MATCH`，参数只允许 `no-resolve` 和 `src`。
- 正则表达式、GEOSITE/GEOIP 名称等由 `mihomo -t` 最终裁定（§1.3 第 2 步）。
- 上限：最多 2000 条，单条最长 1024 个字符。

## 4. IPv6（需求 4）

### 4.1 现状

- `policy::apply` 不写顶层 `ipv6`，原样保留订阅中的值。Mihomo 默认开启 IPv6，但不少订阅显式写了 `ipv6: false`。
- TUN 模式下，如果订阅没有启用 DNS，会注入最小 DNS 块（`policy::minimal_dns_entries`），其中没有 `ipv6`。Mihomo 的 `dns.ipv6` 默认为 false，因此 AAAA 查询返回空，实际上只能用 IPv4。
- mixed 端口监听在 `127.0.0.1`，系统代理不涉及 IPv6，这一点不需要改。

结论：当前对 IPv6 是“取决于订阅、用户无法控制”的状态，所以新增设置。

### 4.2 设置

设置页“网络”分区新增“IPv6”下拉框：

| 取值 | 写入运行配置的内容 |
| --- | --- |
| 跟随订阅（默认） | 不修改，与现状一致 |
| 开启 | `ipv6: true`；如果存在 `dns` 块，再写 `dns.ipv6: true` |
| 关闭 | `ipv6: false`（Mihomo 会阻断 IPv6 连接，并屏蔽 AAAA 记录）；如果存在 `dns` 块，再写 `dns.ipv6: false` |

- 修改后按 §1.3 热重载，不需要重启内核。
- 说明文字：“开启后需要本机和节点都支持 IPv6；TUN 下的 IPv6 接管【待核验】”。这与 DESIGN §11.7 的要求一致：实测通过前，不在 UI 上标记为“已支持”。

## 5. 测速去除握手延迟（需求 5）

### 5.1 原理

现在的测速调用 `GET /proxies/{name}/delay`，测试地址是 `https://cp.cloudflare.com/generate_204`，得到的耗时包含以下几部分：

- 到节点的 TCP 握手和代理协议握手
- 节点到目标网站的 TCP 握手和 TLS 握手
- 一次 HTTP 往返

Mihomo 的 `unified-delay: true` 会在同一条保持连接上连续请求两次，只计第二次的耗时。这样就去掉了上面所有的握手开销，结果接近经过代理的真实往返时间（RTT），也消除了不同协议类型节点之间的口径差异。

### 5.2 设置

- 设置页新增“测速去除握手延迟（统一延迟）”勾选框，默认开启。
  - 开启：强制写入 `unified-delay: true`。
  - 关闭：不修改，跟随订阅。
- 说明文字：“开启后延迟只计算往返时间，数值会明显降低；同时影响自动选择（url-test）组的健康检查。”
- 修改后按 §1.3 热重载；测速缓存的清空沿用现有逻辑（热重载时 `reset_delay_tests`）。
- 测试地址和超时保持不变。
- 顺带修正文档：DESIGN §10.7 写的并发数是 10，代码 `app/delays.rs` 中 `MAX_PARALLEL = 16`，以代码为准，修改文档。

## 6. UWP 回环豁免（需求 2）

### 6.1 背景

UWP 和 Microsoft Store 应用运行在 AppContainer 中，默认不能访问 `127.0.0.1`，所以在系统代理模式下这些应用无法联网。TUN 模式不受影响，入口处要说明这一点。

### 6.2 交互

- 设置页“网络”分区增加“UWP 回环豁免”按钮，点击后打开弹层：
  - 搜索框，以及“全选”“全不选”按钮
  - 列表每行显示：勾选框、显示名、包族名（PackageFamilyName）
  - 底部是“保存（需要管理员授权）”和“取消”
- 打开弹层时以普通权限读取列表；保存时弹出一次 UAC。用户取消 UAC 时按 Cancelled 处理，不改变状态。

### 6.3 读取（普通权限，在主进程中执行）

- 调用 FirewallAPI.dll 的 `NetworkIsolationEnumAppContainers` 获取 `INET_FIREWALL_APP_CONTAINER` 列表，包含 SID、名称、显示名，用完后调用 `NetworkIsolationFreeAppContainers` 释放。
- 显示名可能是 `@{...}` 形式的资源引用，需要用 `SHLoadIndirectString` 解析；解析失败时显示包名。
- 调用 `NetworkIsolationGetAppContainerConfig` 读取当前已豁免的 SID 列表，返回的内存用 `LocalFree` 释放。
- 普通用户权限下这两个调用能否成功【待核验】。如果不能，改为“打开弹层时就提权”（整个弹层由提权后的进程显示），需要重新评估工作量。

### 6.4 写入（提权执行）

- `NetworkIsolationSetAppContainerConfig` 会**整体替换**豁免列表。所以必须提交完整的目标列表（当前已豁免的 + 用户新勾选的 − 用户取消勾选的），这样其他工具设置过的豁免不会被误删。
- 写入方式：复用 `platform/windows/service.rs` 中现有的 `ShellExecuteExW` + `runas` 封装（抽成通用函数），以提权方式启动 `Mirage.exe --set-loopback-exempt <SID>...`，并等待其退出码。
- `main.rs` 中处理这个子命令时：
  - 跳过单实例检查和 UI 初始化
  - 校验每个参数都是 `S-1-15-2-` 开头的合法 SID，用 `ConvertStringSidToSidW` 转换
  - 调用设置接口，返回退出码
- 命令行长度上限是 32767 个字符，大约能容纳 300 个 SID；超出时提示“选择过多”。本轮不改用文件传参。

### 6.5 依赖

`windows` crate 需要新增以下 feature（以 crate 实际划分为准）：

- `Win32_NetworkManagement_WindowsFirewall`
- `Win32_Security_Authorization`
- `Win32_UI_Shell`

### 6.6 工作量评估

| 部分 | 估计 |
| --- | --- |
| FFI 封装：枚举、读取、写入、释放、SID 转换、显示名解析 | 约 200 行 |
| 提权子命令和通用 runas 封装 | 约 80 行 |
| Slint 弹层（列表、搜索、全选）和桥接 | 约 250 行 |
| 合计 | 约 1.5–2 人日 |

- 主要风险是 §6.3 的普通权限读取是否可行。
- 结论：成本中低，作为最后一个独立里程碑（M12）。如果排期紧张，可以只砍掉 M12，不影响其他功能。

## 7. 里程碑

按顺序开发。每个里程碑完成后，先写 `docs/acceptance/<M>.md`，验收通过后再提交。

| 里程碑 | 范围 | 验收要点 |
| --- | --- | --- |
| **M9** 覆写层 + IPv6 + 统一延迟 | §1 基础设施（`UserOverlay`、settings 新字段、§1.3 生效流程），§4，§5；设置页新增两个控件 | 三档 IPv6 和统一延迟开关对应的 run/config.yaml 内容正确；TUN 注入最小 DNS 时 `dns.ipv6` 没有丢失；TUN 服务再次执行策略后这些键仍然保留；开启统一延迟后测速数值明显降低；修改设置后走热重载，没有重启内核 |
| **M10** 规则页框架 + Hosts | 侧栏新增“规则”页（插在“节点”和“设置”之间，`page` 序号顺延，需要同时检查托盘和代码中写死的页码），新增 Lucide 图标；Hosts 标签页；§2 | 格式错误能按行提示；直连域名被正确映射；TUN 下 `nslookup` 结果被映射；**实测经代理节点的连接是否生效**，并据此确定说明文字 |
| **M11** 附加规则 | 附加规则标签页：可视化编辑、YAML 编辑、§3 | 每种可视化规则类型都通过 `mihomo -t`；目标策略不存在时跳过并提示；写 `MATCH` 被拒绝；YAML 与可视化之间切换不丢规则；规则优先于订阅规则命中（在日志页确认命中的规则） |
| **M12** UWP 回环（可砍） | §6 | 普通权限能列出应用；勾选后 Microsoft Store 应用在系统代理模式下可以联网；取消 UAC 不改变状态；其他工具设置过的豁免被保留 |

## 8. 需要同步修改的文档

- DESIGN.md：
  - §9.3 末尾补充“用户覆写层”和键白名单
  - §10.7 更新测速口径和并发数
  - §13 补充 `overrides/` 目录
  - §17.1 页面表格新增“规则”页，设置页新增 IPv6、统一延迟和 UWP 入口
- docs/README.md 的“当前状态”：每个里程碑完成后更新。
- 根 README 的功能列表：在 M11 完成后补充“附加规则 / Hosts / IPv6 / UWP 回环”。

## 9. 待核验清单

| 项 | 在哪个里程碑确认 |
| --- | --- |
| hosts 对经代理节点转发的连接是否生效 | M10 |
| `DOMAIN-WILDCARD` 等较新的规则类型能否通过 v1.19.32 的 `-t` | M11 |
| 普通权限下能否调用 `NetworkIsolationEnumAppContainers` 和 `NetworkIsolationGetAppContainerConfig` | M12 开工前先写最小验证 |
| TUN 下 IPv6 的实际接管效果 | M9（结论只写进验收记录，不在 UI 上标记为“已支持”） |
