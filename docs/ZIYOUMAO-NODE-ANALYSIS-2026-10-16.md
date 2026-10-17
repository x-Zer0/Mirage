# 自由猫节点超时与加密订阅分析（2026-10-16）

本文是对 [ZIYOUMAO.md](ZIYOUMAO.md) 的补充及订阅部分修正。针对 Windows 自由猫 3.1.0+2026072513、Mirage 当前配置和当前账号样本取证；后续实现以本文的订阅结论为准。

## 1. 已确认的原因

**Mirage 下载到的明文订阅与自由猫实际使用的订阅，服务器地址不同。** 当前 `User-Agent: clash-verge` 虽然返回合法 YAML，但其中的节点入口在本次环境中不可用。自由猫使用的 `freecatencua/v` 标识返回 AES 加密订阅，解密后得到另一套节点入口；使用这些节点，官方 Mihomo v1.19.32 实际连接成功。

因此需要补充的是**服务商订阅的解密与正确配置获取**，现有证据不要求修改标准 Mihomo 的 Trojan 传输协议。

原文档“clash-verge 返回标准 YAML，因此可以直接使用”的结论过强：格式检查通过不代表里面的节点可达。此前成功联网使用的是原客户端缓存中的节点，并不能证明明文订阅中的不同服务器地址可用。

### 用户刷新两端后的复核

用户在分析期间重新刷新了自由猫和 Mirage 的节点。重新读取两端落盘配置后：

| 项目 | 自由猫 | Mirage |
| --- | --- | --- |
| 文件修改时间（香港时间） | 2026-10-16 22:31:55 | 2026-10-16 22:31:34 |
| 节点数量 | 118 | 118 |
| 同名节点 | 118 | 118 |
| `server` 分布 | `qaenca1.tobigyang.cn`：52；`qbencb2.tobigyang.cn`：21；`qcencc3.tobigyang.cn`：45 | `freevipa1.freecatnodles.com`：49；b2：3；c3：18；d4：3；e5：18；f6：27（后五个也属于 freecatnodles.com） |
| 逐节点字段差异 | 118 个节点均只有 `server` 不同 | 其他节点字段与自由猫一致 |

这里逐字段比较了节点对象的键和值；同名节点的 `type`、`port`、`password`、`sni`、`skip-cert-verify`、`udp` 全部相同。没有把凭据写入报告。

两份刷新文件 SHA256：

```text
自由猫 config.yaml:
b95cc43713f5871f610dfc7016f81526634755a78259aedee1da998b75c9e243
Mirage 最新 raw revision:
09a44ba8cf0c72afa9e9378587382d9c082d881f1f82ba859abdf10adb559cbd
```

这是刷新后的实际差异，不能归因于旧缓存。证据摘要在 `ziyoumao-analysis/result/refresh-comparison.json`。

## 2. 同一订阅 URL，根据 UA 返回不同内容

使用原客户端已有会话，只读调用 `/user/getSubscribe` 获取订阅 URL，然后针对**同一个 URL**发起两次 GET。订阅请求不携带账号 Authorization：

| User-Agent | 响应内容 | 解析后的节点入口 |
| --- | --- | --- |
| `clash-verge` | 明文 YAML，约 65,062 字节 | 6 个 `freevip*.freecatnodles.com` 域名 |
| `freecatencua/v` | 标准 Base64 字符串，约 114,156 字节 | AES 解密并再次 Base64 解码后，3 个 `q*enc*.tobigyang.cn` 域名 |

两份实时响应均有 118 个同名节点，逐节点比较也只有 `server` 不同，118/118。这独立证明服务器会根据本次请求的客户端标识返回不同入口，而不是 Mirage 的 YAML 合并逻辑把域名改坏。

`freecatencua/v` 来自原版 Flutter AOT 文件 `data/app.so`，ASCII 字符串文件偏移 `0x383040`。这里使用的就是已实测成功的字面量；没有证据要求追加版本号，不自行编造后缀。

原客户端刷新后的缓存与稍后重新获取的加密订阅，有 117 个同名节点，字段完全一致，另有一项节点名称发生变动。这说明服务端内容会动态变化；不要按节点数量、名称或域名做永久映射。

## 3. AES 解密流程：已还原并通过真实样本验证

### 3.1 正常响应的编码层次

本次 `freecatencua/v` 响应的实际流程：

```text
HTTP 响应体（ASCII Base64）
  → 严格 Base64 解码，得到 AES 密文
  → AES-128-CBC 解密
  → 去除 PKCS#7 padding
  → 得到内层 Base64 文本
  → 再次严格 Base64 解码
  → UTF-8 Mihomo YAML
  → YAML 结构检查、Mirage 托管字段覆盖、官方核心校验
```

这些步骤已用 Node 内置 `node:crypto` 对真实在线响应验证成功，解密后交给现有 YAML 解析器，无需新增 Python 加密依赖。

这份响应的 AES 密钥和 IV 是**软件内嵌协议常量**，与账号密码、会话令牌、节点密码不同：

```text
algorithm = AES-128-CBC
key bytes (hex) = 65666266333236633061393436643366
iv  bytes (hex) = 65643238373033333236336431343564
block size = 16 bytes
padding = PKCS#7（真实响应可由标准库自动去除）
```

必须把 hex 转成 16 个原始字节，不能直接把 32 个 hex 字符作为 AES 密钥或 IV。

### 3.2 常量来源与还原方式

下列 VA 属于本次 SHA256 固定的 `ziyoumaoCore.exe`，其 PE ImageBase 为 `0x140000000`。它们是虚拟地址，不是文件偏移；读取时需要经 PE section 映射。

| 符号/数据 | VA | 证据 |
| --- | --- | --- |
| `common/convert.DecodeAESBase64` | `0x140c095c0` | 组装 key/IV、尝试 Base64/hex 解码、调用 CBC 解密、再做 Base64 解码 |
| `common/convert.assembleParts` | `0x140c092a0` | 四段 hex，每段 4 字节；每段与同一个 4 字节 mask 异或后拼接为 16 字节 |
| `common/convert.obfuscationMask` | `0x140c09240` | 读取 hex 编码的 mask |
| `common/convert.aesDecryptCBC` | `0x140c09ca0` | 调用 `crypto/aes.NewCipher`、`crypto/cipher.NewCBCDecrypter` |
| key 的四个 Go string 描述符 | `0x1420587f0`，间隔 `0x10` | descriptor 是 pointer + length |
| IV 的四个描述符 | `0x142058830`，间隔 `0x10` | 同上 |
| mask 描述符 | `0x142058870` | hex `f109929a` |

原始分段常量：

```text
key parts = 946ff0fc c23ba4f9 c168abae c76da1fc
iv  parts = 946da0a2 c639a1a9 c33fa1fe c03da7fe
mask      = f109929a
key/iv    = concat(hex_decode(part) XOR hex_decode(mask), for each part)
```

地址只适用于本次二进制。分析工具在提取前核对核心 SHA256，遇到不同版本会拒绝沿用这些地址。正式客户端应实现本次确认的协议解码，不应依赖用户安装目录或二进制地址。

### 3.3 不要混淆三类封装

| 内容 | 本次结论 | Mirage 应如何处理 |
| --- | --- | --- |
| 在线 `freecatencua/v` 响应 | Base64 → AES-CBC → Base64 → YAML，已实测 | 作为自由猫专用订阅解码步骤 |
| 原客户端 `config.yaml` | 自定义字符映射 Base64，已实测 | 保留已有只读导入支持 |
| 原客户端 `profiles/999999.yaml` | 二进制本地存储封装，本次未完整还原 | 在线实现无需依赖它 |

`DecryptConfig`（VA `0x140c12ac0`）的静态路径包含 YAML、AES、普通 Base64、映射 Base64 等识别分支。原程序的容错分支不是新客户端必须逐字复制的契约；尤其不要在 AES 失败后把密文当成有效配置。

## 4. 节点怎样使用，是否还要加密

解密后的节点仍然是标准 Trojan over TLS。样本的节点字段只有：

```yaml
proxies:
  - name: sample
    type: trojan
    server: <从解密订阅原样读取>
    port: <从订阅读取>
    password: <从订阅原样读取>
    sni: <从订阅原样读取>
    skip-cert-verify: <保持订阅值>
    udp: <保持订阅值>
```

此模板中的尖括号是占位说明，不是可运行配置。样本没有显式指定其他 `network`。118 个节点的 SNI 都不等于各自 `server`，不能用服务器域名覆盖 SNI，也不能将节点 UUID 形式密码预先做 AES/Base64/SHA 运算再写入配置。

静态核查到的关键传输函数：

| 函数 | VA | 已观察行为 |
| --- | --- | --- |
| `transport/trojan.Key` | `0x140bb3c80` | 调用 `crypto/sha256.Sum224`，按小写 hex 生成 Trojan 认证值 |
| `transport/trojan.WriteHeader` | `0x140bb2220` | 写入认证值、分隔符、命令和目标地址 |
| `outbound.(*Trojan).streamConnContext` | `0x140caca40` | 序列化 SOCKS 目标地址，调用 Trojan WriteHeader |
| `outbound.(*Trojan).DialContext` | `0x140cacec0` | 建立连接，再进入 TLS/流封装 |

认证散列和 TLS 由标准 Mihomo 自动处理；Mirage 只需完整传递解密后的节点对象。静态符号不足以证明整个定制核心与上游完全相同，但下面的标准核心实际联网结果支持“这些已测节点不需要定制传输层”。

## 5. 实际连接对照

使用官方 Mihomo v1.19.32；4 个订阅中的真实节点，索引 0、6、30、61，覆盖解密订阅的三个入口域名及一个当前原核心已建立连接所用的端口。每组使用独立本地进程、相同 DNS、相同检测 URL，关闭 TUN，不设置系统代理。

检测请求为控制器 `GET /proxies/<name>/delay?url=https%3A%2F%2Fcp.cloudflare.com%2Fgenerate_204&timeout=8000`；测试别名为 probe-0/6/30/61，不输出真实密码。

| 配置来源 | 出口 | 四个节点结果（ms） |
| --- | --- | --- |
| AES 解密订阅 | 默认出口 | 1158、3171、1683、2526，均 HTTP 200 |
| AES 解密订阅 | 显式 `interface-name: 以太网` | 534、1193、1175、884，均 HTTP 200 |
| clash-verge 明文订阅 | 默认出口 | 全部 HTTP 504 |
| clash-verge 明文订阅 | 显式物理网卡 | 全部 HTTP 504 |

随后补做物理网卡下的 HTTPS 请求验证：

| 配置来源 | 再次单节点测速 | 经本地 mixed-port 发起 HTTPS 请求 |
| --- | --- | --- |
| AES 解密订阅 | 512、1158、1014、967 ms，均 HTTP 200 | `https://cp.cloudflare.com/generate_204` 返回 **204** |
| clash-verge 明文订阅 | 504、503、504、504 | 请求失败（URLError），核心有 connect error |

只替换订阅来源就改变了同批节点的结果，说明“缺少正确入口配置”足以解释本次失败；仅增加测速超时或替换检测网站不能解决这个差异。

### DNS 与 TUN 的观察边界

- Mirage 2026-10-16 22:18–22:19 的历史日志出现：`freevipa1.freecatnodles.com:29833 connect error: context deadline exceeded`，并有 DNS 请求失败。它不是只有 UI 显示超时。
- 通过物理网卡向 `223.5.5.5` 查询时，6 个 `freevip*.freecatnodles.com` 域名返回了 CNAME `google.com` 和 Google 地址；3 个 `tobigyang.cn` 域名返回另一组 CNAME/入口地址。这是本次 DNS 观测，未查明是服务商配置还是 DNS 污染，不将归因写死。
- 当前自由猫 TUN 在线；普通系统 DNS 查询甚至返回 `198.18.0.0/16` 的 fake IP。最初直接用系统 DNS 做 socket 测试时，两类节点都超时，因此这种未隔离 DNS 的结果不能拿来否定节点兼容性。
- 有效对照使用 Mihomo 自身的节点 DNS，并增加了物理网卡绑定组；两类订阅的成功/失败差异仍然存在。
- 本次没有关闭原客户端/TUN，没有宣称已完成原客户端完全退出后的独立验收，也没有验证全部 118 节点、UDP、IPv6、TUN 或长期稳定性。

## 6. Mirage 后续实现要求

本次交付是分析文档与复核工具，未修改客户端业务代码。建议下一步实现如下：

1. 保留已有登录、`auth_data`、getSubscribe 和双令牌隔离流程。
2. 将自由猫的下载 UA 改为本次已验证的 `freecatencua/v`，同时明确输出格式为“自由猫 AES 订阅”，不要把加密响应直接交给通用 YAML 检测器。
3. 在 `providers/ziyoumao` 或其拥有的订阅转换边界，完成严格 Base64 → AES-128-CBC → padding 检查 → 内层 Base64 → UTF-8/YAML。复用项目现有 Base64、HTTP 限额、秘密存储和配置管线，新增加密依赖前先核查已有依赖。
4. 解码后仍进入通用 compose、官方 `-t`、版本持久化和原子应用流程。保持节点对象字段完整，不硬编码域名替换表。
5. 失败时报告订阅格式/解密不兼容，保留上一个可用版本；不要自动降级到本次已证实不可达的明文入口，再向用户宣称更新成功。
6. 处理当前已经下载的明文旧 revision：改 UA 后重新获取、解密、校验并切换版本。只改 UA 而继续使用旧配置不会修复现有连接。
7. `GET /proxies/<name>/delay` 可以用于单节点定位；现有组测速把所有 5xx 当作成功且 UI 把 delay=0 统一叫超时，会掩盖 DNS、TLS、连接失败等原因。建议保留具体错误与失败阶段，不把所有 5xx 都解释成节点超时。
8. 连接前的冲突提示与单独退出原自由猫后的验收仍有必要。不要把物理接口名称“以太网”硬编码成产品默认值，它只用于本机对照。

解密层应归属于自由猫来源，避免使普通订阅解析器默认接受任意 AES 数据。新的格式描述如何接入现有 DownloadDescriptor/ConfigFormat，需要在正式修改时核对调用链。

## 7. 可复查证据与工具

工作区 `ziyoumao-analysis/` 下新增：

| 文件 | 用途 |
| --- | --- |
| `analyze_subscription.py` | 只读获取两种 UA 响应，从已核验核心提取常量并解密，默认仅输出脱敏摘要和节点差异 |
| `probe_subscription.py` | 生成临时对照配置、启动官方核心、测速和 HTTPS 检测；摘要不输出凭据 |
| `result/refresh-comparison.json` | 用户刷新两端后的落盘样本对比 |
| `result/live-ua-comparison.json` | 同一订阅 URL 两种 UA 的实时对比 |
| `result/subscription-probes.jsonl` | 两种订阅 × 两种出口的四组测速记录 |
| `result/subscription-probes-https.jsonl` | 物理出口补充测速与 mixed-port HTTPS 结果 |
| `result/config-crypto.asm.txt` | AES 组装、解密路径的地址化反汇编 |
| `result/transport-and-config.asm.txt` | Trojan 认证与配置解码相关反汇编 |

默认摘要复核命令，在工作区根目录执行：

```powershell
python -B -X utf8 .\ziyoumao-analysis\analyze_subscription.py
```

此原型会读取原客户端保存的 `flutter.auth_token` 并只用于已经发现的账号 API，不使用保存密码。`--emit-config` 会显式输出含节点凭据的解密 YAML，仅用于后续分析；不要将其输出提交到仓库或放进日志。`probe_subscription.py --prepare` 的输出也包含真实节点配置，应仅由 PowerShell 写入受限 ACL 临时目录，验证结束后清空；它不是普通的无敏感输出命令。

本次验证的临时 YAML 已清空，所有本次启动的 Mihomo 进程已停止；没有修改原客户端安装文件、原客户端配置、Mirage 账号或配置数据，也没有更改系统代理、路由或停掉原客户端。

### 样本身份

```text
ziyoumao.exe SHA256:
a8fd983d61e0706b1077fc5b306cc2e0cdf9ab071beec789c4c6371350412fe0
app.so SHA256:
e64526b7fa64d609070188764e7d16f9de05ae6239a90bdd527e81e992625ed0
ziyoumaoCore.exe SHA256:
0742e91e784b65f9071eb96967d038ccf51e703321b79d511bcc404efafa391a
官方 Mihomo SHA256:
beb9878924d7bd38c67176b441ab5e668cc378bfe751c8dc8fef3eb5aaf566d5
```

分析参考了本地 REA 的 `reverse-engineer-anything` 技能流程，但本次 REA MCP 未连接，ZIP 内 CLI 也缺少编译运行时。新增静态证据实际由已有 PE/Go 函数表解析器、Capstone 和普通文件读取生成；动态证据来自真实 HTTP 请求及官方 Mihomo，未冒称为新生成的 REA Evidence。

未编写或运行单元测试。分析脚本完成 Python AST 语法检查，Markdown 链接与事实摘要经过复核。
