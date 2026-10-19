# Mirage 代码评审报告

**日期**：2026-10-19  
**版本**: master 分支 `54eec3d` (feat：日志界面重做)  
**评审范围**：全部 113 个 Rust 文件（约 16,000 行）、16 个 Slint 文件、构建与打包脚本、全部设计/验收文档。

**静态基线**：
- `cargo fmt --all --check` ✓  
- `cargo check --workspace --locked` ✓  
- `cargo clippy --workspace --all-targets --locked -- -D warnings` ✓  
- `Cargo.lock` 无重复版本，依赖全部精确锁定

---

## 总体评价

架构与设计要求吻合度很高：
- ✅ `mirage-ipc` 共享策略避免了应用与服务漂移，§9.3 的强制覆盖表逐项落实
- ✅ §10.4 的"挂起创建→入 Job→恢复"顺序正确
- ✅ §11.3 的事务顺序（先写日志再改 WinINet）正确
- ✅ 凭据不落元数据、日志脱敏层设计合理

**问题集中在三类**：

1. **状态机边界**：少量异步结果没有纳入 generation/事务模型，导致 UI 与内核实际状态可能不一致
2. **失败路径的提交顺序**：几处"先破坏后写入"，失败时会丢失恢复能力或用户数据
3. **热路径上的全量比较/重建**：日志页和节点列表在 10,000 条规模下会产生明显 UI 开销

---

## 一、高危问题（建议优先修复）

### H1. 启动过程中切换配置不会生效，也不会取消启动

**位置**: `crates/mirage-app/src/app/profile_actions.rs:160`

**问题**:  
`select_profile` 只在 `self.running` 有值时才重启内核。`running` 在 `on_started` 里才被赋值，所以 `Starting` 期间（最长 15 秒）切换配置只保存了选择：旧启动任务继续用旧 `Profile`，`generation` 未变，`on_started` 的校验通过并安装旧配置。

**失败场景**:  
用户在内核启动过程中点击切换到配置 B，等待 15 秒后连接成功，界面显示配置 B，但实际运行的是配置 A，无任何提示，只有重新连接才恢复一致。

**修复建议**:  
`Starting` 时递增 `generation` 使旧启动任务作废（旧结果会走 `Ok(stale)` 分支被停止），再以新配置启动：

```rust
// profile_actions.rs select_profile
if self.running.is_some() || self.view.runtime == RuntimeStatus::Starting {
    self.generation += 1;  // 作废旧启动
    if let Some(previous) = self.running.take() {
        self.view.groups.clear();
        self.connection_source = "切换配置";
        self.start_after(Some(previous));
    } else {
        // Starting 期间没有 running，start_after(None)
        self.connection_source = "切换配置";
        self.start_after(None);
    }
}
```

---

### H2. 内核异常退出绕过 TUN 状态回退，`proxy_before_tun` 被永久钉住

**位置**: `crates/mirage-app/src/app/supervise.rs:59`

**问题**:  
`on_core_exited` 直接写 `self.view.tun_active = false`，而正常路径都走 `tun.rs:10` 的 `update_tun_state(false)`。

**失败场景**:  
内核意外崩溃，`proxy_before_tun` 不被清空。重启成功后 `update_tun_state(true)` 因 `proxy_before_tun.is_some()` 跳过快照与 `tun_disable_system_proxy` 的自动关闭；之后再关 TUN，会恢复一个来自崩溃前的陈旧代理偏好。同时 TUN 关闭这条操作日志也不会记录。

**修复建议**:  
改为统一路径：

```rust
// supervise.rs on_core_exited 第59行
- self.view.tun_active = false;
+ self.update_tun_state(false);
```

---

### H3. 系统代理已写入但通知失败时，恢复日志被删除

**位置**: `crates/mirage-app/src/app/sysproxy.rs:72-74` + `platform/windows/sysproxy.rs:97-108`

**问题**:  
`write()` 依次执行"写值 → SETTINGS_CHANGED → REFRESH"。前两步中任意一步失败（写值已成功）都会返回 `Err`，`enable()` 随即删除 `recovery/sysproxy.json`。

**失败场景**:  
系统代理已指向本地端口，但恢复记录没了；进程若在此后崩溃，下次启动 `restore()` 返回 `Nothing`，机器被留在指向死端口的代理上——正是恢复日志存在的目的。

**修复建议**:  
把写值作为提交点；两个通知调用改为尽力而为：

```rust
// platform/windows/sysproxy.rs write()
unsafe {
    InternetSetOptionW(/* ... */)
        .map_err(io::Error::other)?;
    // 提交点：设置已写入
    let _ = InternetSetOptionW(None, INTERNET_OPTION_SETTINGS_CHANGED, None, 0);
    let _ = InternetSetOptionW(None, INTERNET_OPTION_REFRESH, None, 0);
}
Ok(())
```

或者返回一个"部分成功"错误并保留 `Applied` 日志，由调用方决定是否继续。

---

### H4. 凭据覆盖先删除旧值再写新值

**位置**: `crates/mirage-app/src/platform/windows/secret.rs:35-37`

**问题**:  
短值路径先 `remove_file(key)` 再 `cred_write`。`CredWriteW` 失败（配额、凭据库损坏）时旧订阅 URL / 会话已删除、新值未写入。

**失败场景**:  
用户的账号和订阅地址静默丢失，配置永远无法验证。

**修复建议**:  
先 `cred_write` 成功，再删除旧 DPAPI 文件；外部文件路径失败时回收刚写的文件：

```rust
// secret.rs put()
pub fn put(&self, key: &str, value: &[u8]) -> AppResult<()> {
    if value.len() <= CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
        cred_write(key, value)?;  // 先写
        self.remove_file(key);     // 成功后删旧
        return Ok(());
    }
    let sealed = dpapi(value, true)?;
    std::fs::create_dir_all(&self.dir).map_err(|e| AppError::storage(STAGE, e))?;
    crate::storage::atomic::write(&self.file_for(key), &sealed)
        .map_err(|e| AppError::storage(STAGE, e))?;
    match cred_write(key, EXTERNAL_MARKER) {
        Ok(()) => Ok(()),
        Err(e) => {
            self.remove_file(key);  // 回滚
            Err(e)
        }
    }
}
```

---

### H5. 日志页"暂停滚动"会被模型刷新立即撤销

**位置**: `ui/pages/logs.slint:81` + `ui_bridge/models.rs:50-60,406-417`

**问题**:  
`paused` 由 `ListView.content-y` 推导，而 `apply_logs` 在连接批次到达时整体 `set_connection_logs` 重建模型。模型替换后滚动位置回到 0，`content-y` 变化把 `paused` 改回 `false`。

**失败场景**:  
用户点暂停后几秒内就会被新连接撤销；向上滚动同样会被拉回顶部。

**修复建议**:  
1. `paused` 只由按钮写入；滚动只用于"未暂停时感知用户上滚"，且不要回写同一属性。
2. 后端 `refresh_logs` 在暂停时应按 `anchor` 截断或跳过重建：

```rust
// app/log_actions.rs refresh_logs
if self.log_query.paused {
    page.connections = Arc::new(/* 按 anchor 截断后的列表 */);
} else {
    // 正常路径
}
```

```slint
// logs.slint
changed content-y => {
    if !root.paused && self.content-y < -1px {
        // 用户上滚，可能想固定视图，但只记录不回写 paused
    }
}
```

---

### H6. YAML 别名放大（订阅内容不可信）

**位置**: `crates/mirage-app/src/subscriptions/detect.rs:80`、`providers/ziyoumao/legacy_import.rs:52-65`、`providers/ziyoumao/subscription.rs:41`

**问题**:  
`serde_yaml_ng` 的别名实现会重新展开锚定子树，只受"跳转次数 ≤ 事件数×100"约束。16 MiB 上限发生在展开之前，嵌套别名可以在一秒内产生数十倍于原文的 `Value` 节点，阻塞或 OOM actor 运行时。

**失败场景**:  
恶意订阅通过深嵌套别名使解析时间/内存暴涨，主 actor 被阻塞数秒甚至 OOM。

**修复建议**:  
解析前做廉价文本门控（限制别名定义数/引用数），或改用支持节点数上限的解析方式；三条路径共用同一函数：

```rust
// detect.rs / ziyoumao/
fn safe_yaml_parse(text: &str) -> AppResult<Value> {
    // 门控：别名引用总数 ≤ 100
    let alias_count = text.matches(" *").count() + text.matches(" &").count();
    if alias_count > 100 {
        return Err(AppError::new(AppErrorKind::UnsupportedFormat, "detect", "YAML 别名过多"));
    }
    serde_yaml_ng::from_str(text).map_err(|e| /* ... */)
}
```

或切换到带节点数上限的解析库（如果 `serde_yaml_ng` 未来支持）。

---

### H7. 热路径上的全量深比较与整组重建

**位置**:  
- `crates/mirage-app/src/app/actor.rs:170-185`  
- `ui_bridge/models.rs:50-60, 206-228`

**问题**:  
1. `publish()` 在主循环每轮都执行 `**current == self.view`，日志页打开时每 5 秒的代理轮询要对最多 10,000 条记录做全量 `PartialEq`。
2. `apply` 里 `p.logs != s.logs`、`p.groups != s.groups` 同样是深比较，发生在 UI 线程。
3. `update_groups` 的"按行更新"传入的是新建的 `GroupRow`，其 `nodes` 是全新的 `VecModel`；任意一个节点延迟变化都会重建整组（最多 1000 行）。

**失败场景**:  
10,000 条日志的 UI 在每次轮询时卡顿，节点列表每次延迟刷新都重建所有子模型。

**修复建议**:  
1. `LogsPage`/`groups` 加单调 `revision` 字段，比较改为比较 revision
2. `publish` 用显式脏标记（如 `view_dirty: bool`）
3. 分组持有稳定的 `Rc<VecModel<NodeRow>>`，只对变化行 `set_row_data`：

```rust
// state.rs
#[derive(Clone, PartialEq, Eq)]
pub struct LogsPage {
    pub revision: u64,  // 新增
    pub connections: Arc<Vec<ConnectionLog>>,
    // ...
}

// actor.rs publish
if self.view_dirty {  // 新增标记
    *current = Arc::new(self.view.clone());
    self.view_dirty = false;
    true
} else {
    false
}

// models.rs update_groups
// 传入稳定的 Rc<VecModel<NodeRow>>，只 set_row_data 变化行
```

---

## 二、中危问题

### M1. `GetExtendedTcpTable` 错误码判断不完整

**位置**: `crates/mirage-app/src/platform/windows/conflict.rs:184-197`

**问题**:  
`GetExtendedTcpTable` 返回 `ERROR_INSUFFICIENT_BUFFER(122)`，代码判断 `ERROR_BUFFER_OVERFLOW(111)`，放大分支不执行。

**失败场景**:  
监听表超过 16 KiB 时端口占用提示丢失进程名。

**修复**:  
```rust
if status == ERROR_BUFFER_OVERFLOW.0 || status == ERROR_INSUFFICIENT_BUFFER.0 {
    buf.resize(size as usize, 0);
}
```

---

### M2. provider 缺省 `type` 被拒

**位置**: `crates/mirage-ipc/src/policy.rs:272-285`

**问题**:  
provider 缺省 `type` 被当作不支持并报 `PolicyRejected`；Mihomo 对缺省按 `http` 处理（验证确认：`parser.go` 的 `schema.Type` 默认为 `""`，落入 `default` 分支报错），合法订阅不应被拒。

**更正**：经 Mihomo v1.19.32 源码验证，`schema.Type` 缺失时确实走 `default` 分支并报 `errVehicleType`。当前实现正确。

**不需修复**（保留拒绝逻辑）。

---

### M3. 校验输出先截取再找错误

**位置**: `crates/mirage-app/src/cores/mihomo/check.rs:56-74`

**问题**:  
先截取前 64 行再找 `error/fatal`，警告多时把一条警告当作失败原因。

**修复**:  
保留尾部窗口，同时始终保留 error 行：

```rust
let mut lines: Vec<String> = /* stdout + stderr */;
let errors: Vec<_> = lines.iter().filter(|l| l.contains("level=error") || l.contains("level=fatal")).cloned().collect();
lines.truncate(MAX_LINES);
lines.extend(errors.into_iter().take(3));  // 优先保留 error
```

---

### M4. 账号刷新故障转移不完整

**位置**: `crates/mirage-app/src/providers/ziyoumao/mod.rs:98-102`

**问题**:  
可信主机返回 HTML 挑战页时 `ProviderCompat` 直接中断，健康备用主机不再尝试。

**修复**:  
只有 `AuthInvalid`/`NeedsUserAction` 提前返回，其余记 `last_err` 继续：

```rust
Err(e) if matches!(e.kind, AppErrorKind::AuthInvalid | AppErrorKind::NeedsUserAction) => {
    return Err(e);
}
Err(e) => last_err = Some(e),
```

---

### M5. 主机发现全量并发

**位置**: `crates/mirage-app/src/providers/ziyoumao/bootstrap.rs:192-232`

**问题**:  
每个候选主机建一个任务、全部跑完才选；恶意 bootstrap 可用上万条主机拖住登录。

**修复**:  
候选上限（如 16）+ 首个成功即返回：

```rust
let bases: Vec<_> = candidates(hosts).into_iter().take(16).collect();
// 改用 tokio::select! 等待首个 Ok 即返回
```

---

### M6. URL 路径脱敏不完整

**位置**: `crates/mirage-app/src/domain/redact.rs:20-38`

**问题**:  
只脱敏 query 参数，不处理路径中的 token；§14.3 "完整订阅 URL 只保留前 8 字符路径"未实现。

**修复**:  
增加 URL 路径裁剪规则：

```rust
rule(r"(https?://[^/]+/)[^?#]{8,}", "${1}********"),
```

---

### M7–M25（其余中危问题）

见完整列表（保存失败回滚、generation 检查、健壮性提升等）。

---

## 三、低危与工程卫生（L1–L25）

**略**（详见原报告）。主要涵盖：Base64 换行、版本排序、userinfo 边界、HTTP 客户端复用、锁中毒容错、UI 状态修正、构建声明等。

---

## 四、冗余与可抽取的公共方法

1. **配置保存**：`updates.rs:132-192` 与 `profile_actions.rs:76-135` 重复实现"存版本→写列表→记操作日志→通知"，且两处的保存失败回滚行为不一致（见 M8）。建议抽 `commit_new_profile(profile, raw, revision, source)`。

2. **detect→compose 重复**：`runtime.rs:218-250` 的 `apply_revision` 重新实现了 detect→compose，绕过了 `pipeline::prepare`，与 §8"全工程只实现一份"相悖。建议 `prepare` 增加 `check: bool`。

3. **操作日志**：`record_operation + 从错误生成通知` 的成对调用重复约 12 次，可抽 `report(op, obj, src, result)`。

4. **GiB 格式化**：`state.rs:90` 与 `profile_view.rs` 完全相同。

5. **进程身份**：`mirage-svc/src/identity.rs` 与 `platform/windows/process.rs` 各有一份（服务端注释已说明原因，可接受），但 `same_path` 等纯函数可上移到 `mirage-ipc`。

6. **重复文件**：`packaging/ChineseSimplified.isl` 与 `packaging/licenses/Inno-ChineseSimplified.isl` 逐字节相同。

7. **`.gitignore`**: 未忽略 `.claude/`。

---

## 五、文档与实现不一致

| 文档 | 描述 | 实现 |
|---|---|---|
| DESIGN §13 | 先 `ReplaceFileW`，目标不存在再 `MoveFileExW` | 只用 `MoveFileExW`；替换后文件继承临时文件属性，与 ACL 语义略有偏差 |
| DESIGN §14.1 | `SecretStore::get` 返回 `Zeroizing<Vec<u8>>` | 返回 `Vec<u8>` |
| DESIGN §7 | 队列满时提示"操作过于频繁" | 只记 warn 日志 |
| DESIGN §17.4 | 节点延迟/选择按行更新 | 实际按组重建（H7） |

**建议**：按实际实现更新 DESIGN §13（原子写）与 §14.1（SecretStore 签名），或让实现符合文档；更新 §7、§17.4 中与实现不符的描述。

---

## 六、需要确认的设计问题

1. **别名限制的边界**：订阅中合法的锚点/别名很常见（合并键、节点模板）。你希望是"限制别名引用总数"还是"彻底改用节点数上限"？这决定 H6 的实现方式。

2. **启动时到期配置同时更新**：冷启动时若有 10 个配置都过期，是希望全部并发（当前部分并发）还是排队逐个更新？影响 M10 的修复形态。

3. **崩溃重启后的 TUN 语义**：内核崩溃后自动重启成功，是否应重新应用 `tun_disable_system_proxy`（即视为一次新的 TUN 会话）？设计未写明，H2 的修复依赖这个答案。

4. **配置日志按日分文件的时区**：当前用 UTC 命名文件、UI 用本地时间分组，UTC+8 的凌晨记录会落到前一天的文件。你希望统一成本地时区吗？

5. **服务连接模型**：`mirage-svc` 同一时刻只接受一个 owner 连接，第二个连接会排队等待。是否需要支持并发（例如诊断与主会话同时查询状态）？

---

## 建议直接修改清单（请逐项确认）

### A 批 — 高危，建议全部直接改

- [ ] A1 H1 启动中切换配置：递增 generation 使旧启动作废
- [ ] A2 H2 内核异常退出改走 `update_tun_state(false)`
- [ ] A3 H3 系统代理通知失败保留恢复日志
- [ ] A4 H4 凭据先写新值再删旧值
- [ ] A5 H5 日志暂停只由按钮控制 + 暂停时后端按 anchor 截断
- [ ] A6 H6 YAML 别名门控（三条路径共用）
- [ ] A7 H7 `publish` 脏标记 + 日志/分组 revision 比较

### B 批 — 中危，建议直接改

- [ ] B1 M1 `port_owner` 错误码
- [ ] B2 ~~M2 provider 缺省 type 按 http~~（验证后无需修改）
- [ ] B3 M3 校验输出保留 error 行
- [ ] B4 M4 账号刷新故障转移
- [ ] B5 M5 主机发现上限与短路
- [ ] B6 M6 URL 路径脱敏
- [ ] B7 M7 设置保存失败回滚
- [ ] B8 M8 新增配置失败回滚
- [ ] B9 M9 节点选择串行写
- [ ] B10 M11–M14 操作日志防抖/容错/时区、schema 截断
- [ ] B11 M15 release 去掉开发回退路径
- [ ] B12 M16–M18 UI 模型比较、队列提示、开关回弹
- [ ] B13 M19–M20 凭据 Zeroizing、对齐修正
- [ ] B14 M21–M22 重载 generation 与 Invalid 判定
- [ ] B15 M23–M25 诊断哈希、路径防线、日志初始化失败提示

### C 批 — 低危清理

- [ ] C1 L1–L4 订阅解析边界
- [ ] C2 L5 HTTP 客户端复用
- [ ] C3 L6–L10 账号/TUN/重载/busy 状态修正
- [ ] C4 L11–L15 内核侧小幅优化与健壮性
- [ ] C5 L16–L19 服务端健壮性（锁中毒、卸载顺序）
- [ ] C6 L20–L25 UI 状态与构建声明
- [ ] C7 冗余抽取（第四节 1–5）
- [ ] C8 `.gitignore` 忽略 `.claude/`；移除重复的 `ChineseSimplified.isl`

### D 批 — 文档同步

- [ ] D1 按实际实现更新 DESIGN §13（原子写）与 §14.1（SecretStore 签名），或让实现符合文档
- [ ] D2 更新 §7、§17.4 中与实现不符的描述

---

## 附录：静态检查指令

```powershell
cargo fmt --all --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

---

**评审完成，尚未修改任何代码**。请告诉我可以直接执行哪些批次，以及第六节五个问题的答案。我会在修改后统一跑 `fmt / check / clippy`，并按你的要求不新增单元测试。
