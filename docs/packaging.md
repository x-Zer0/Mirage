# Windows 安装版、便携版与分发（M8）

目标为 Windows 10 1809+ / Windows 11 x64。同一次打包构建同时生成安装版 EXE 和免安装 ZIP，二者复用同一批 release 文件、官方核心及许可证材料。安装版使用 Inno Setup；默认安装到 `%LOCALAPPDATA%\Programs\Mirage`，安装本身无需 UAC。安装向导包含欢迎页、安装说明、目录选择、可选桌面快捷方式、完成说明和启动选项；开始菜单及 Windows 应用列表提供卸载入口。

## 构建

使用 PowerShell 7、x86_64-pc-windows-msvc Rust 1.92+ 和 MSVC C++ 构建工具。`.cargo/config.toml` 为 Windows x64 启用 `crt-static`，主程序及服务都不依赖另外安装 VC Runtime。

准备 Inno Setup 6.4.3 和 cargo-about 0.9.2（工具无需入库）：

- [Inno Setup 官方 release](https://github.com/jrsoftware/issrc/releases/tag/is-6_4_3)，安装器 SHA256：`f3c42116542c4cc57263c5ba6c4feabfc49fe771f2f98a79d2f7628b8762723b`。也可用 `/PORTABLE=1 /CURRENTUSER /DIR="<工具目录>"` 解压为便携工具。
- [cargo-about 官方 release](https://github.com/EmbarkStudios/cargo-about/releases/tag/0.9.2)，x86_64-pc-windows-msvc.tar.gz SHA256：`1c03e5890238562497c2d89a3b75b02560af349c1fc3e713d3284f532a5cd748`。
- 本机工具分别位于 `target/tools/innosetup-6.4.3/` 和 `target/tools/cargo-about/`，脚本自动探测；也可传入绝对路径。

```powershell
pwsh -File scripts/package.ps1
# 工具在其他位置时：
pwsh -File scripts/package.ps1 -Iscc 'D:\Tools\Inno Setup\ISCC.exe' -CargoAbout 'D:\Tools\cargo-about.exe'
```

脚本校验并复用已下载的官方核心 / geodata，缺失时按固定 SHA256 下载；锁定依赖构建整个 workspace，生成许可证清单，再编译安装包并使用 PowerShell 自带 `Compress-Archive` 生成便携 ZIP，无需额外压缩工具。首次执行 cargo-about 若提示缺少其他平台 crate，先在联网环境执行一次 `cargo fetch --locked`，之后许可证生成使用 `--offline --locked --fail`。

输出：

- `dist/Mirage-<版本>-windows-x64-setup.exe`
- `dist/Mirage-<版本>-windows-x64-portable.zip`
- 两个产物分别附同名 `.exe.sha256` / `.zip.sha256` 校验文件。
- `target/package/<版本>-<随机标识>/` 留存独立打包目录，包含安装版公共文件及 `payload-sha256.txt`；其下 `portable/Mirage/` 为便携版目录，独立清单还覆盖便携版说明，便于核对实际入包文件。用于安装器编译的 `app.ico` 不额外放入 ZIP。

`-SkipBuild` 仅用于已完成本次 release 构建后的重新打包；`-StageOnly` 只准备公共目录、不调用安装器编译器，也不输出 EXE / ZIP；正常构建和 `-SkipBuild` 重打包均同时输出两种产物。`cargo build` 本身仅编译程序，分发统一使用上述打包入口。版本来自 Cargo metadata；主程序和服务版本必须一致，核心哈希必须与 `crates/mirage-ipc/src/binary.rs` 一致。四份 geodata 和核心压缩包的固定值集中在 `scripts/core-assets.psd1`，下载和打包复用它。geodata 下载固定到上游 [2026-10-21 快照提交](https://github.com/MetaCubeX/meta-rules-dat/tree/eed74d50aa6083d028110ed3feaf37f5e3070d9d)，使用该提交的 raw 文件地址及各自 SHA256，不受 latest release 滚动更新影响。更新时必须同时核对提交 ID、四份文件哈希和随包来源说明。

## GitHub Actions

云端流程复用同一打包脚本：推送到 master / 提交 PR 时进行静态检查，手动运行生成 Artifacts，推送与 Cargo 版本一致的 vX.Y.Z 标签后自动发布 GitHub Release。工具安装、权限、缓存与操作步骤见 [GitHub Actions 构建与发布](github-actions.md)。

## 随包内容与首次使用

```text
Mirage.exe
mirage-svc.exe
core/
  mihomo.exe
  GEOIP.metadb
  GEOSITE.dat
  GEOIP.dat
  ASN.mmdb
licenses/
  Mirage-MIT.txt
  rust-dependencies.html
  Slint-Royalty-free-2.0.md
  Slint-components.txt
  Mihomo-GPL-3.0.txt
  Lucide-LICENSE.txt
  geodata-notices.txt
  CC-BY-SA-4.0.txt
  Inno-Setup.txt
THIRD-PARTY-NOTICES.txt
payload-sha256.txt
```

安装过程不创建、启动或升级 MirageSvc。TUN 每次启动默认关闭。首次开启 TUN，或者检测到服务归属路径 / 用户、应用版本、helper SHA256 不匹配时，先显示安装 / 更新说明，确认后再 UAC。只有授权及安装成功后才继续启用；取消保持原有模式。普通订阅、系统代理模式无需安装服务，离线安装后可导入本地 YAML。

辅助服务经现有安装 helper 复制到 `%ProgramFiles%\Mirage\mirage-svc`，核心和 geodata 同时进入受保护目录，服务保持 LocalSystem / Manual。升级后首次开启 TUN 再更新受保护副本；安装器不会为此预先提权。服务仍绑定单个安装用户和 Mirage.exe 的规范路径。

## 升级和卸载

安装器通过 `Mirage.exe --quit` 请求正常退出，等待其完成系统代理恢复和核心停止；关闭到托盘设置不会阻止退出。无法在 30 秒内退出则停止安装，不强杀。没有 UI 时也会按恢复日志还原 Mirage 自己设置的系统代理，并核对身份后清理遗留核心；外部改动的代理保持现状。旧的 M0～M7 裸 exe 未实现此命令，迁移前请先从托盘退出。

卸载前执行 `Mirage.exe --prepare-uninstall`：正常退出及恢复完成后，只清理属于当前用户、当前安装路径的服务；服务卸载请求 UAC，取消或失败则中断卸载、保留应用文件。再删除指向本 exe 的开机启动项。Windows 卸载器删除应用文件及快捷方式，保留 `%LOCALAPPDATA%\Mirage` 的订阅、凭据和偏好，便于重装；不会清空用户数据目录。

静默安装可使用 Inno 标准 `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART`，静默安装不会自动启动应用。静默卸载若存在本应用服务，仍需要可交互的管理员授权；不提供绕过 UAC 的模式。

## 便携版使用、更新与移除

ZIP 内只有顶层 `Mirage/` 文件夹；完整解压后运行其中的 `Mirage.exe`，保留 `mirage-svc.exe`、`core/` 和 `licenses/`。额外附带的 `README-portable.txt` 说明数据位置、TUN、更新和移除步骤。解压不创建快捷方式、Windows 应用卸载项或辅助服务。普通代理、订阅、节点、测速等运行逻辑与安装版一致；首次启用 TUN 仍提示安装 / 更新服务并请求 UAC。

这是主程序免安装分发。设置、订阅、缓存和日志仍在 `%LOCALAPPDATA%\Mirage`，与同一用户的安装版共用；同一用户仅运行一个 Mirage 实例。账号及订阅凭据使用 Windows 凭据管理器 / 当前用户 DPAPI，不随程序目录自动迁移到其他电脑或用户；不引入程序旁的 `data/` 模式。

更新前从托盘正常退出，或运行 `.\Mirage.exe --quit`，等待代理恢复和内核退出，再完整替换文件。移动目录后手动启动可修复已启用的开机启动项；服务绑定用户及客户端路径，下一次开启 TUN 可能再次请求 UAC 更新路径 / 版本。安装版和便携版共用同名服务，切换运行路径也适用此规则。

移除前在设置中关闭开机启动；若安装过 TUN 服务，关闭 TUN 后在设置中卸载服务（需要管理员授权），再从托盘退出并删除程序目录。只删除文件夹不会清理服务、启动项或用户数据；同一用户的安装版也会使用这些数据和服务，移除前需确认是否仍要保留。

## 许可证与签名

- Mirage 自身代码采用 [MIT 许可证](../LICENSE)，打包时复制为 `licenses/Mirage-MIT.txt`，安装版与便携版均随附。
- Mihomo 为未修改的官方 v1.19.32，随包附 GPLv3 原文、准确版本源码 tag / archive / release 地址。公开分发时，应在二进制下载处提供完整对应源码的取得方式，包括其依赖和构建材料；不能只保留内部文档中的链接。
- Slint 使用 Royalty-free 2.0，诊断页已有 AboutSlint；随包带原文。cargo-about 对 LicenseRef 自定义名称不生成正文段，Slint 的正文、版本和组件声明由独立文件提供；`about.toml` 用原文 SHA256 固定选择依据，不把它错误标成 GPL 或 MIT。
- Rust 依赖清单覆盖 Windows x64 workspace 的运行时依赖，排除纯构建 / 开发依赖及 `publish=false` 的本项目代码。普通 SPDX 许可证正文、归属、版本和源码地址由 cargo-about 自动生成；未知许可会中止打包。
- geodata 保留上游项目和数据来源归属，GeoLite2 ASN 附 MaxMind、CC BY-SA 4.0 声明。Lucide、安装器的许可也随包提供。
- 当前产物未签名，不生成自签证书，不宣称已通过 SmartScreen。正式签名证书可用后再加入 Authenticode 流程，并启用 §11.7 的签名身份校验。

实测记录和待验收范围见 [M8 验收记录](acceptance/M8.md)。
