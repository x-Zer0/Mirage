# GitHub Actions 构建与发布

工作流位于 [build-release.yml](../.github/workflows/build-release.yml)，复用 [package.ps1](../scripts/package.ps1) 构建 Windows x64 安装版和便携版。

## 触发与执行顺序

| 触发方式 | 静态检查 | 打包并上传 Artifacts | 创建 GitHub Release |
| --- | --- | --- | --- |
| 推送到 `master` | 是 | 否 | 否 |
| 面向 `master` 的 Pull Request | 是 | 否 | 否 |
| Actions 页面手动运行 | 是 | 是 | 否 |
| 推送 `vX.Y.Z` 标签 | 是 | 是 | 是，自动公开发布 |

`check → package → release` 按顺序执行，任一阶段失败都会阻止后续阶段。手动运行即使选择已有标签也只打包。普通分支不触发 push 构建，可通过 Pull Request 或手动运行检查。

- **check**：在 `windows-2022` 上执行 `cargo fmt --all --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`。不运行单元测试。
- **package**：固定 Rust **1.95.0 x86_64 MSVC**（与现有本机验收一致），下载并校验 Inno Setup **6.4.3**、cargo-about **0.9.2**。先运行 `cargo fetch --locked`，确保离线许可证生成所需的依赖齐全，再调用现有打包脚本；核心和 geodata 也由该脚本校验。
- **release**：在独立 job 中下载本次构建产物，使用内置 `GITHUB_TOKEN` 发布正式 Release，附自动生成的变更说明及 `THIRD-PARTY-NOTICES.txt` 中的来源、许可证与 Mihomo 对应源码链接。

geodata 固定使用上游 [2026-10-21 快照提交](https://github.com/MetaCubeX/meta-rules-dat/tree/eed74d50aa6083d028110ed3feaf37f5e3070d9d) 的 raw 文件，并逐份核对 SHA256。地址和哈希集中在 `scripts/core-assets.psd1`，全新 runner 也能下载同一批资源；更新资源时需同步随包来源说明。构建使用仓库中的 `Cargo.lock` 和 `crt-static` 配置。Rust 依赖缓存按检查 / release 用途分开；Pull Request 只恢复缓存，不写入缓存。各个 Action 固定到已核对的提交 SHA，升级时需同步 SHA 和旁边的版本注释。标签任务不自动取消，日常检查在同一分支有新运行时取消旧运行。

## 首次使用

1. 将 workflow 和配套改动提交并推送到 `master`，在仓库 **Actions** 页面确认 `Build and release` 检查通过。手动运行入口需要 workflow 已位于默认分支。
2. 在该工作流页面点击 **Run workflow**，选择待验证分支。成功后，在运行详情底部 **Artifacts** 下载 `Mirage-<版本>-windows-x64`，确认安装版和便携版可用。
3. 正式发布时更新根目录 `Cargo.toml` 的 `[workspace.package].version`，并同步、提交 `Cargo.lock` 中的 workspace 版本。先将版本提交推送到 `master` 并确认检查通过。
4. 在版本提交上创建标签并单独推送该标签，例如当前版本 `0.1.0`：

```powershell
git switch master
git pull --ff-only
git tag -a v0.1.0 -m 'Mirage 0.1.0'
git push origin v0.1.0
```

标签必须严格等于 `v` 加 workspace 版本，例如 `v0.1.0` 对应 `0.1.0`。`v*` 标签都会触发检查，格式错误或版本不匹配会在打包前失败。当前打包脚本仅接受三个数字段的正式版本，不支持 `-beta` / `-rc` 标签。

## 产物与权限

每次手动打包和标签发布都会上传以下四个文件：

```text
Mirage-<版本>-windows-x64-setup.exe
Mirage-<版本>-windows-x64-setup.exe.sha256
Mirage-<版本>-windows-x64-portable.zip
Mirage-<版本>-windows-x64-portable.zip.sha256
```

Actions Artifacts 保留 **14 天**；正式版本也会作为 Release 附件保存，不受该保留期限制。Artifacts 外层归档解压后，便携 ZIP 仍需单独完整解压。SHA256 校验方法：

```powershell
Get-FileHash .\Mirage-0.1.0-windows-x64-setup.exe -Algorithm SHA256
Get-Content .\Mirage-0.1.0-windows-x64-setup.exe.sha256
```

无需配置个人 PAT 或额外 Secrets。工作流默认只有 `contents: read`，仅 `release` job 声明 `contents: write`。仓库 / 组织需允许 GitHub Actions、所用的固定 Action 提交和托管 runner；若发布返回 403，检查组织权限策略和标签 / Release 保护规则。不要为此把 PAT 放入 YAML。

当前安装包沿用项目的未签名状态；签名接入后需要在最终压缩、生成 SHA256 前完成，以保证校验值对应实际发布文件。

## 失败后处理

- 静态检查失败：按日志修正格式、编译或 Clippy 问题，再推送提交。
- 资源或工具 SHA256 不符：停止发布，核对下载来源和固定值；更新资源时同时更新来源、固定值及分发说明。
- 打包成功、发布失败：确认权限后可在运行页面重试失败的 job，复用本次 Artifacts；需在 14 天保留期内完成。
- 已正式发布的版本发现问题：修复后递增版本并创建新标签，避免移动旧标签或替换用户已下载的同名产物。

本地 workflow 语法检查不等于 GitHub 托管 runner 的实际执行。首次推送后应先完成一次手动打包，再创建正式标签。
