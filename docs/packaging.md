# Windows 安装包与分发（M8）

目标为 Windows 10 1809+ / Windows 11 x64。使用 Inno Setup；默认安装到 `%LOCALAPPDATA%\Programs\Mirage`，安装本身无需 UAC。安装向导包含欢迎页、安装说明、目录选择、可选桌面快捷方式、完成说明和启动选项；开始菜单及 Windows 应用列表提供卸载入口。

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

脚本校验并复用已下载的官方核心 / geodata，缺失时按固定 SHA256 下载；锁定依赖构建整个 workspace，生成许可证清单，再编译安装包。首次执行 cargo-about 若提示缺少其他平台 crate，先在联网环境执行一次 `cargo fetch --locked`，之后许可证生成使用 `--offline --locked --fail`。

输出：

- `dist/Mirage-<版本>-windows-x64-setup.exe`
- 同名 `.exe.sha256` 校验文件。
- `target/package/<版本>-<随机标识>/` 留存独立打包目录，包含 `payload-sha256.txt`，便于核对本次实际入包文件。

`-SkipBuild` 仅用于已完成本次 release 构建后的重新打包；`-StageOnly` 只准备目录、不调用安装器编译器。版本来自 Cargo metadata；主程序和服务版本必须一致，核心哈希必须与 `ipc/src/binary.rs` 一致。四份 geodata 和核心压缩包的固定值集中在 `scripts/core-assets.psd1`，下载和打包复用它，滚动数据变化时拒绝静默替换。

## 随包内容与首次使用

```text
Mirage.exe
mirage-service.exe
core/
  mihomo.exe
  GEOIP.metadb
  GEOSITE.dat
  GEOIP.dat
  ASN.mmdb
licenses/
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

安装过程不创建、启动或升级 MirageService。TUN 每次启动默认关闭。首次开启 TUN，或者检测到服务归属路径 / 用户、应用版本、helper SHA256 不匹配时，先显示安装 / 更新说明，确认后再 UAC。只有授权及安装成功后才继续启用；取消保持原有模式。普通订阅、系统代理模式无需安装服务，离线安装后可导入本地 YAML。

辅助服务经现有安装 helper 复制到 `%ProgramFiles%\Mirage\service`，核心和 geodata 同时进入受保护目录，服务保持 LocalSystem / Manual。升级后首次开启 TUN 再更新受保护副本；安装器不会为此预先提权。服务仍绑定单个安装用户和 Mirage.exe 的规范路径。

## 升级和卸载

安装器通过 `Mirage.exe --quit` 请求正常退出，等待其完成系统代理恢复和核心停止；关闭到托盘设置不会阻止退出。无法在 30 秒内退出则停止安装，不强杀。没有 UI 时也会按恢复日志还原 Mirage 自己设置的系统代理，并核对身份后清理遗留核心；外部改动的代理保持现状。旧的 M0～M7 裸 exe 未实现此命令，迁移前请先从托盘退出。

卸载前执行 `Mirage.exe --prepare-uninstall`：正常退出及恢复完成后，只清理属于当前用户、当前安装路径的服务；服务卸载请求 UAC，取消或失败则中断卸载、保留应用文件。再删除指向本 exe 的开机启动项。Windows 卸载器删除应用文件及快捷方式，保留 `%LOCALAPPDATA%\Mirage` 的订阅、凭据和偏好，便于重装；不会清空用户数据目录。

静默安装可使用 Inno 标准 `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART`，静默安装不会自动启动应用。静默卸载若存在本应用服务，仍需要可交互的管理员授权；不提供绕过 UAC 的模式。

## 许可证与签名

- Mihomo 为未修改的官方 v1.19.32，随包附 GPLv3 原文、准确版本源码 tag / archive / release 地址。公开分发时，应在二进制下载处提供完整对应源码的取得方式，包括其依赖和构建材料；不能只保留内部文档中的链接。
- Slint 使用 Royalty-free 2.0，诊断页已有 AboutSlint；随包带原文。cargo-about 对 LicenseRef 自定义名称不生成正文段，Slint 的正文、版本和组件声明由独立文件提供；`about.toml` 用原文 SHA256 固定选择依据，不把它错误标成 GPL 或 MIT。
- Rust 依赖清单覆盖 Windows x64 workspace 的运行时依赖，排除纯构建 / 开发依赖及 `publish=false` 的本项目代码。普通 SPDX 许可证正文、归属、版本和源码地址由 cargo-about 自动生成；未知许可会中止打包。
- geodata 保留上游项目和数据来源归属，GeoLite2 ASN 附 MaxMind、CC BY-SA 4.0 声明。Lucide、安装器的许可也随包提供。
- 当前产物未签名，不生成自签证书，不宣称已通过 SmartScreen。正式签名证书可用后再加入 Authenticode 流程，并启用 §11.7 的签名身份校验。

实测记录和待验收范围见 [M8 验收记录](acceptance/M8.md)。
