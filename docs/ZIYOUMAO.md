# 自由猫（ziyoumao）接入规范

版本 v1.0（2026-10-15）。本文是 `src/providers/ziyoumao/` 的实现依据，自包含：实现时无需再查阅逆向材料。通用接口（`ProviderAccountAdapter`、`DownloadDescriptor`、`SecretStore`、`HttpClientFactory`、错误类型）定义见 [DESIGN.md](DESIGN.md) §5、§6、§12、§14。

本文不是官方 API 文档。接口来自原客户端静态分析、公开 Web 客户端源码和授权账号的只读实测，服务端行为可能变化。证据标记：

- 【已验证】有实测记录。
- 【源码确认】在公开 Web 客户端源码中看到，未实测。
- 【待核验】推测或未观察到。

---

## 1. 结论与总流程

```text
bootstrap（公开 Base64 JSON）→ 候选主机 → GET /guest/comm/config 验证主机
  → POST /passport/auth/login（email/password）→ data.auth_data
  → GET /user/info、GET /user/getSubscribe（Authorization: <auth_data 原值>）
  → data.subscribe_url（含独立订阅 token）
  → GET subscribe_url（User-Agent: clash-verge，无 Authorization）
  → 标准 Mihomo YAML → 通用配置管线
```

- 【已验证】订阅在线路径返回的是标准明文 Mihomo YAML，官方 Mihomo v1.19.32 `-t` 退出码为 0。单节点经官方内核请求 `generate_204` 返回 204。因此不需要自由猫定制内核，也不需要节点协议转换。
- 原客户端的内核通过本地 TCP 加换行分隔 JSON 的私有 action 协议通信，不是官方 Mihomo CLI/REST 接口，所以不能把官方 exe 直接替换进原客户端的安装目录。Mirage 使用标准 CLI 和 REST 接口。

## 2. 证据表

| 项目 | 状态 | 结论 |
| --- | --- | --- |
| 原客户端 | 3.1.0+2026072513，Flutter Windows | — |
| 原核心 | Go 1.25.0 windows/amd64 with_gvisor；mihomo 依赖 replace 到 ./Clash.Meta | 基于 Mihomo，但有私有封装，不能证明没有其他修改 |
| bootstrap 入口 | 【已验证】HTTPS 200，内容为普通 Base64 编码的 JSON | 可以取得 hosts |
| 两个域名主机的 `/guest/comm/config` | 【已验证】200 | 主机验证方法可行 |
| `/user/info`、`/user/getSubscribe` | 【已验证】使用保存的会话令牌时返回 200 | Authorization 直接使用令牌原值 |
| 2026-10-16 主机发现复测 | 【已验证 M4】入口 Base64 JSON 可直连读取；两个域名主机 `/guest/comm/config` 均 200，IP 主机按规则跳过 | 无现有代理时也可直连 |
| 订阅下载 | 【已验证】UA 为 `Clash.Meta` 时返回 567，UA 为 `clash-verge` 时返回 200 明文 YAML（Content-Type: text/html） | 必须指定 UA；不能依据 Content-Type 判断 |
| 订阅样本 | 118 个 Trojan 节点，3 个组，516 条规则，0 个 provider；节点字段 name/password/port/server/skip-cert-verify/sni/type/udp | 仅是当时的样本，不得硬编码 |
| 官方核心校验 | 【已验证】v1.19.32 `-t` 退出码 0，带有 `fallback-filter.geosite` 弃用警告 | 警告不影响使用 |
| 账号密码登录 | 【已验证 M4，失败路径】JSON `{"email","password"}` 被接受（未返回 415/422）；不存在的账号返回 **HTTP 500** + JSON 信封 `message: "邮箱或密码错误"` | 成功响应的字段名待用真实账号登录时记录 |
| 刷新令牌 / 续期 | 【待核验】没有找到 refresh_token 或刷新接口的证据 | 不实现续期 |
| 服务端注销 | 【待核验】未确认接口 | v1 只清除本地数据 |
| TUN/UDP/IPv6 | 未验证 | 不宣称支持 |

## 3. 模块划分

```text
src/providers/ziyoumao/
  mod.rs            # ZiyoumaoAdapter: ProviderAccountAdapter
  bootstrap.rs      # 入口下载、解码、主机候选、缓存
  api.rs            # 请求构造、响应信封解析、错误映射、DTO
  auth.rs           # 登录、会话验证
  account.rs        # /user/info + /user/getSubscribe → AccountSnapshot
  subscription.rs   # subscribe_url → DownloadDescriptor
  legacy_import.rs  # 可选：原客户端 config.yaml 只读解码
```

- 模块输出 `AccountSnapshot`、`DownloadDescriptor` 和 `AppError`。
- 不负责 UI 跳转、启动内核、修改系统代理，也不把自由猫特有字段放进通用节点模型。
- 数据按 `account_id` 隔离，不使用全局单例 token。v1 的 UI 只支持一个自由猫账号，但数据层支持多个。

---

## 4. 主机发现（bootstrap.rs）

### 4.1 入口

```text
GET https://flmain-1326556767.cos.ap-guangzhou.myqcloud.com/ConFigOss.json
```

处理步骤：

1. 使用 `Bootstrap` 客户端：不跟随重定向，响应上限 256 KiB，超时 20 秒。
2. 去除首尾空白，按标准 Base64（含 `=` padding）严格解码。
3. 解码结果按 UTF-8 JSON 解析。
4. 读取 `hosts` 数组。

```rust
#[derive(Deserialize)]
struct BootstrapDoc {
    hosts: Vec<String>,
    #[serde(rename = "Authentication")] authentication: Option<String>, // 观察值“自由猫”，用途未知，不使用
    host_source: Option<String>,   // 公共导航地址，不使用
    // InviteLink / Customer / UpdateUrl 等：原客户端的其他功能入口，忽略
}
```

当时观察到的 hosts，仅供参考，不得硬编码为唯一来源：

- `https://tccdnclient.miaowuppaa.com`：【已验证】
- `https://tuc.skyoutdoors.com.cn`：【已验证】
- `https://115.159.221.23`：IP 主机，证书无法匹配，未验证。

### 4.2 主机筛选

- 只接受满足以下条件的地址：`https://` 开头、主机部分为域名、没有 userinfo、没有 query 或 fragment。IP 字面量主机直接跳过，不能为了使用它而关闭证书校验。
- 按顺序对每个主机请求 `GET <host>/api/v1/guest/comm/config`，不带令牌。如果返回 200、是 JSON 且 `data` 为对象，就选用这个主机，API base 为 `<host>/api/v1`。
- 并发探测最多 2 个主机，取最先成功的一个。全部失败时报 `Network`，并附上每个主机的失败阶段（已脱敏）。

### 4.3 缓存与信任

- 成功后写入 `providers-cache/ziyoumao/bootstrap.json`：`{hosts, selected, fetched_at}`，不包含任何令牌。
- bootstrap 入口失败时使用缓存的主机列表；缓存也失败时报网络错误。网络失败与账号失效是两回事，不得改变账号状态。
- 会话与主机的绑定：登录成功时，把当时的 `selected` 写入 `ProviderAccount.trusted_hosts`。之后携带该账号令牌的请求只能发往 `trusted_hosts` 中的主机。
- bootstrap 后来返回新主机时：
  - 新主机可以用于无令牌请求和新的登录。
  - 已有会话不得自动转发到新主机。
  - 如果所有可信主机都不可达，而 bootstrap 中出现了新主机，账号状态设为 `NetworkUnavailable`，UI 提示“服务地址已变更，请重新登录”。用户重新登录后，新主机才会加入可信列表。
- 登录 POST 只发往一个已选主机，不跟随重定向，不广播到多个主机。

## 5. 接口契约（api.rs）

以下路径都以 `<API base> = <host>/api/v1` 为前缀。账号类请求使用 `ProviderApi("ziyoumao")` 客户端：不跟随重定向，响应上限 1 MiB，总超时 20 秒，出站策略按 DESIGN.md §12.2。

| 操作 | 方法与路径 | 请求 | 关键响应字段 | 状态 |
| --- | --- | --- | --- | --- |
| 公共配置 | `GET /guest/comm/config` | 无令牌 | `data`（对象） | 【已验证】 |
| 登录 | `POST /passport/auth/login` | JSON `{"email","password"}` | `data.auth_data` | 【源码确认】 |
| 账号信息 | `GET /user/info` | `Authorization: <auth_data>` | `data.email`、`transfer_enable`、`expired_at`、`banned`、`plan_id` | 【已验证】 |
| 订阅信息 | `GET /user/getSubscribe` | 同上 | `data.subscribe_url`、`u`、`d`、`transfer_enable`、`expired_at`、`plan`、`alive_ip` | 【已验证】 |
| 下载订阅 | `GET <subscribe_url>` | `User-Agent: clash-verge`，不带 Authorization | 标准 YAML | 【已验证】 |

所有账号类请求的公共头：`Accept: application/json`。登录请求额外带 `Content-Type: application/json`。

### 5.1 响应信封

接口风格属于 V2Board 一类面板。【已验证 M4】登录失败的响应是 JSON 信封，`message` 为可展示的中文原因；状态码为 500（V2Board `abort(500, …)` 风格），不是 4xx。成功响应的形状仍待真实账号登录时记录。

```rust
#[derive(Deserialize)]
struct Envelope<T> {
    data: Option<T>,
    message: Option<String>,   // 错误提示（推测）
    errors: Option<serde_json::Value>,
}
```

解析规则：

- HTTP 2xx 并且 `data` 存在且类型正确时，判为成功。
- HTTP 2xx 但 `data` 为空或类型不对时，报 `ProviderCompat`，记录 `message`（脱敏、截断到 200 字符）。不能把 HTTP 200 等同于业务成功。
- 非 2xx 按 §8 映射为错误。
- 响应必须是 JSON，否则报 `ProviderCompat`（“非 JSON 响应”）。

### 5.2 DTO

所有字段都是 `Option`，类型宽松：数字字段同时接受数字和数字字符串。

```rust
#[derive(Deserialize)]
struct LoginData { auth_data: Option<String> /* 其他字段首次登录时记录 */ }

#[derive(Deserialize)]
struct UserInfo {
    email: Option<String>,
    transfer_enable: Option<Num>,   // 总流量，单位推测为字节【待核验】
    expired_at: Option<Num>,        // 推测为 Unix 秒；null 的含义【待核验】
    banned: Option<NumOrBool>,
    plan_id: Option<Num>,
}

#[derive(Deserialize)]
struct SubscribeInfo {
    subscribe_url: Option<String>,
    u: Option<Num>, d: Option<Num>, // 上行/下行已用流量（推测为字节）
    transfer_enable: Option<Num>,
    expired_at: Option<Num>,
    plan: Option<PlanInfo>,         // 至少取 name
    alive_ip: Option<Num>,          // 在线设备数（推测）
}
```

映射到 `AccountSnapshot`：

- `used_bytes = u + d`。
- `total_bytes = transfer_enable`。
- `expires_at = expired_at`，值为 null 或 0 时显示“未提供”，不显示为“永久”。
- `plan_name = plan.name`。
- `email_masked`：脱敏后的邮箱。

单位和时间的含义在首次实测时与原客户端界面对照确认，结果写回本节。

---

## 6. 登录与会话（auth.rs）

### 6.1 登录请求

```http
POST /api/v1/passport/auth/login
Content-Type: application/json
Accept: application/json

{"email":"<用户输入>","password":"<用户输入>"}
```

- 【已验证 M4】服务端接受 JSON 编码（用不存在的账号实测：返回业务错误而不是 415/422）。成功响应中 `data` 的字段名在首次真实账号登录时由日志 `ziyoumao login response fields=…` 记录（只记字段名）。
- 编码方式采用 JSON，与 Python 原型一致。原则上：如果服务端拒绝 JSON（例如返回 415 或 422，并提示字段缺失），把错误信息展示给用户并记录下来，然后人工决定是否改为表单编码。不得自动用多种编码重复提交密码。
- 登录只由用户主动触发，失败时不自动重试。
- 密码以 `Zeroizing<String>` 保存在内存中，请求发出后立即清零，永不写入磁盘。
- 请求体不得出现在日志、错误信息、panic 信息或 Debug 输出中。

### 6.2 会话令牌

- `auth_data` 是不透明字符串。不要假设它是 JWT，也不要自行解析有效期。
- 后续请求使用 `Authorization: <auth_data 原值>`，不加 `Bearer ` 前缀（公开 Web 客户端也是这样使用的）。
- 登录成功后把令牌写入 `SecretStore`，键为 `Mirage/account/<account_id>/session`。v1 默认保存会话，因为用户添加账号的目的就是长期使用。移除账号时删除这条记录。

### 6.3 验证码与挑战

公共配置中可以看到 `is_email_verify`、`is_invite_force`、`email_whitelist_suffix`、`is_recaptcha`、`recaptcha_site_key` 等字段。这些字段可能只用于注册流程，不能据此推断登录一定需要验证码。

- 登录前读取公共配置。如果 `is_recaptcha` 为真，UI 提示“该服务可能需要人机验证，若登录失败请稍后重试或使用原客户端完成验证”。仍然允许用户尝试登录。
- 登录响应如果明确要求验证码或设备确认，进入 `NeedsUserAction(提示文本)`。无法识别的挑战报“不支持的验证方式”。
- 不绕过、不伪造验证流程，也不嵌入 reCAPTCHA。

### 6.4 会话状态机

```text
LoggedOut ──login──▶ LoggingIn ──Session──▶ Valid
                       ├─Rejected──▶ LoggedOut（显示原因）
                       ├─NeedsUserAction──▶ NeedsUserAction
                       └─Network──▶ 保持原状态（原来有会话就保留），显示网络错误
Valid ──validate──▶ Validating ──▶ Valid / ReauthRequired / NetworkUnavailable
```

- 应用启动时，如果有已保存的会话，就调用 `GET /user/info` 验证（后台执行，不阻塞连接）。
- 只有 401 或 403，或业务信封明确表示未登录或令牌失效时，才转为 `ReauthRequired`。网络错误和 5xx 转为 `NetworkUnavailable`，保留令牌。
- 没有刷新接口时，不要在后台循环提交密码来“保活”。会话失效后提示用户重新登录。
- 账号状态变化不影响正在运行的内核（DESIGN.md §5.1）。
- 移除账号只清除本地数据：删除令牌、订阅 URL 和账号元数据，对应 Profile 由用户决定是否一并删除。服务端注销接口尚未确认，v1 不调用。

## 7. 订阅（subscription.rs）

### 7.1 两类令牌隔离

| 令牌 | 用途 | 存储键 |
| --- | --- | --- |
| `auth_data` | 账号 API 的 Authorization | `Mirage/account/<id>/session` |
| `subscribe_url` 中的 token 参数 | 订阅下载 | `Mirage/account/<id>/subscribe_url` |

- 订阅下载请求不得携带账号 Authorization、Cookie 或密码。
- 账号 API 客户端和订阅下载客户端是不同的 `reqwest::Client` 实例，没有共享的默认请求头。
- 【已验证】当时观察到的订阅主机为 `frontsubapi.miaowuppaa.com`，路径为 `/api/v1/client/subscribe?token=...`。该主机可能随服务端调整而改变，不能写死。只校验它是 `https://` 地址、主机部分为域名、没有 userinfo。
- 完整订阅 URL 按秘密处理：只在用户点击“复制”时放进剪贴板，日志中做脱敏。

### 7.2 生成下载描述

```rust
DownloadDescriptor {
    url: subscribe_url,
    headers: vec![(USER_AGENT, "clash-verge")],
    expected_formats: vec![ConfigFormat::MihomoYaml],
    expires_at: None,
    redirect: RedirectPolicy::None,
}
```

- UA 必须严格为 `clash-verge`，这是实测通过的值。不要追加版本号，以免触发服务端的 UA 规则。这个 UA 只用于自由猫，不改变普通订阅的默认 UA。
- 每次更新订阅时先调用 `/user/getSubscribe` 获取最新的 URL，然后再下载。URL 变化时更新 SecretStore 中的记录。不能假设订阅 token 永远不变，也不能假设它每次都会轮换。
- `getSubscribe` 因网络问题失败时，可以使用 SecretStore 中已保存的上一次 URL 下载，并在日志中标注“使用缓存的订阅地址”。如果是鉴权失败，不再继续下载，改为提示用户重新登录。

### 7.3 下载结果判定

- 【已验证】成功响应的 Content-Type 是 `text/html; charset=UTF-8`，但内容实际是 YAML。所以 Content-Type 不参与判断，直接按 DESIGN.md §8 第 3 步做内容识别。
- 反过来，HTTP 200 加 HTML 错误页或挑战页的情况由 `detect` 识别为 `NotAConfig`，不会被保存为可用配置。
- 内容识别通过后进入通用管线，节点密码、SNI、`skip-cert-verify` 等服务商字段保持原样。
- 当时的订阅带有 `fallback-filter.geosite` 弃用警告，校验仍然通过。升级内核前要重新做一次校验，不要擅自修改订阅中的 DNS 段。
- 不检查节点数量、协议、组名或规则数量。服务商以后改用其他标准协议时，也照常走标准校验。

---

## 8. 错误映射与重试

| 情况 | AppErrorKind | 账号状态 | 重试 |
| --- | --- | --- | --- |
| DNS/连接/TLS 失败、超时 | `Network`/`Tls`/`Timeout` | 不变或 `NetworkUnavailable` | GET 请求：退避 1s、3s，最多 2 次 |
| 登录：HTTP 4xx 或 500 且信封有 message（如密码错误；【已验证 M4】实际为 500 + “邮箱或密码错误”） | `LoginRejected` | `LoggedOut`（已有会话保留） | 不自动重试 |
| 登录：HTTP 415/422 | `ProviderCompat`（“服务端不接受登录请求的格式”） | 不变 | 不重试，不改用其他编码 |
| 登录：要求验证码或设备确认 | `NeedsUserAction` | `NeedsUserAction` | 等待用户操作 |
| 账号 API：401/403 或明确的“未登录” | `AuthInvalid` | `ReauthRequired` | 不重试 |
| 429 | `RateLimited{retry_after}` | 不变 | 按 `Retry-After` 等待（上限 60 秒），只等一次；登录不重试 |
| 订阅：567 | `ProviderCompat` | 不变 | 不重试；提示“服务端拒绝了客户端标识” |
| 5xx | `Http(code)` | 不变 | GET 请求同网络错误的重试规则 |
| 2xx 但信封或结构不符 | `ProviderCompat` | 不变 | 不重试 |
| 下载内容为 HTML 或空 | `NotAConfig` | 不变 | 不重试 |
| 校验失败 | `ConfigCheckFailed` | 不变 | 保留旧版本 |

- 未知错误不得显示为“密码错误”。
- 所有重试都可取消，并在 DESIGN.md §6.5 的 generation 控制之下进行。
- POST 请求（登录）一律不自动重试，避免重复登录挤掉会话或触发限流。
- 下载失败时保留上一个可用版本。不能用旧的下载描述覆盖新会话的信息。

## 9. 原客户端配置导入（legacy_import.rs，可选功能）

用于迁移或在离线状态下恢复，需要用户在 UI 中明确选择“从原自由猫客户端导入”后才执行。

- 默认路径：`%APPDATA%\ziyoumao\ziyoumao\config.yaml`。用户也可以手动选择文件。
- 只读取，不修改或删除原客户端的任何文件。
- 【已验证】这个文件的格式是：先把标准 Base64 字母表替换为一张映射字母表，再进行编码。

```text
标准：ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/
映射：ZYXWVUTSRQPONMLKJIHGFEDCBAzyxwvutsrqponmlkjihgfedcba9876543210/+
```

即大写字母表反转、小写字母表反转、数字表反转，`+` 和 `/` 互换，`=` padding 保持不变。

解码流程：

```rust
fn decode_legacy(raw: &[u8]) -> Result<String, AppError> {
    let text = strip_bom(std::str::from_utf8(raw)?);
    if looks_like_mihomo_yaml(text) { return Ok(text.into()); }            // 已经是明文
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if let Ok(s) = b64_strict_utf8(&compact) { if looks_like_mihomo_yaml(&s) { return Ok(s); } } // 普通 Base64
    let mapped: String = compact.chars().map(map_char).collect();          // 映射 → 标准
    let s = b64_strict_utf8(&mapped)?;
    if looks_like_mihomo_yaml(&s) { Ok(s) } else { Err(UnsupportedFormat) }
}
// map_char: 'A'..='Z' → ('Z' as u8 - (c - 'A')) ；'a'..='z' 同理；'0'..='9' → '9' - (c - '0')；'+' ↔ '/'；其他原样
```

- 每种解码方式最多尝试一次，不做无限或递归解码。
- 解码成功后，作为 `LocalFile` 来源（名称为“自由猫导入”）进入通用管线，并且必须通过 `-t` 校验。Base64 解码成功并不代表配置有效。
- 不支持的情况：原核心中的 `DecodeAESBase64` / `DecryptConfig` 变体，没有拿到样本进行验证；`profiles/999999.yaml` 有另外一层封装，尚未还原。遇到这两种情况时报 `UnsupportedFormat`。
- 不读取原客户端的 `shared_preferences.json`（其中可能保存了密码和会话）。导入原客户端的会话不属于 v1 范围。

## 10. 实现顺序与验收

按顺序实现，每一步完成后都做手工验证，并记录到 `docs/acceptance/M4.md`：

1. bootstrap 加主机验证：在诊断页显示候选主机和每个主机的验证结果（不需要令牌）。
2. 真实账号首次登录：记录请求编码是否被接受、成功响应中 `data` 的字段名（只记录字段名，不记录值）、密码错误时的状态码和信封形状。把结果写回本文 §5.1、§6.1，并改为【已验证】。
3. 会话保存到 SecretStore，重启后能恢复并通过验证；手动吊销会话后（例如在网页端修改密码），确认得到的响应并补全 §8 的映射。
4. 获取订阅：确认通过独立客户端和指定 UA 能下载到 YAML，并进入通用管线。
5. 完全退出原自由猫客户端（包括它的 TUN），再用 Mirage 连接，至少一个节点能通过 `generate_204` 检测。
6. 依次验证窗口、托盘、系统代理的恢复。

| 场景 | 预期 |
| --- | --- |
| 正确账号首次登录 | 进入 Valid，显示套餐和流量 |
| 密码错误 | 明确提示，不重试，日志中没有请求体 |
| 需要验证 | 进入 NeedsUserAction，有提示文本 |
| 重启后恢复会话 | 无需输入密码即可读取账号与订阅 |
| 令牌失效 | 进入 ReauthRequired，内核不受影响 |
| 断网或主机不可达 | 进入 NetworkUnavailable，会话和旧配置都保留 |
| 订阅返回 567、429 或 HTML | 有界处理，不写入活动配置 |
| 200 + text/html + YAML 正文 | 通过识别和校验后接受 |
| 新版本校验失败 | 保留旧版本，显示原因 |
| 日志与诊断包 | 搜索不到密码、auth_data、订阅 token、节点密码 |

## 11. 仍需确认

- 首次登录成功时的响应结构（编码与失败响应已在 M4 确认）。
- 令牌的有效期、并发登录限制、设备绑定方式。
- 是否有验证码或设备确认流程。
- 订阅 URL 是否会轮换。
- `expired_at` 为 null 以及流量字段的单位和含义。
- 无现有代理（原客户端的 TUN 已关闭）时，bootstrap 和 API 能否直连访问（M4 测试时原客户端仍在运行，未能排除其 TUN 的影响）。
- 服务端是否提供 sing-box 格式的订阅。未确认之前，不得编造 `flag=sing-box` 之类的参数。

每确认一项，就把结果写回本文对应章节。

## 附：原客户端参考信息

- 数据目录：`%APPDATA%\ziyoumao\ziyoumao\`，包含 `config.yaml`、`shared_preferences.json`、`profiles/`。
- 进程：`ziyoumao.exe`（Flutter UI）、`ziyoumaoCore.exe`（定制内核）。用于冲突检测，见 DESIGN.md §11.8。
- 公开 Web 客户端：https://us.freecat.cloud/ 。其中资源文件名带内容哈希，会随版本发布而变化，需要核对协议细节时可再次查看。

