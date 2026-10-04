# Git Bash / MinGit：对应源码与交付状态

核对日期：2026-10-04。仅适用于 Git for Windows `v2.55.0.windows.5` /
`MinGit-2.55.0.5-64-bit.zip` 的本地 366 文件快照，不可沿用到其他版本。
官方 [MinGit Release](https://github.com/git-for-windows/git/releases/tag/v2.55.0.windows.5)
公布的二进制 SHA-256：
`56d7b226b7693196cfc71fef26568f536c4a021ab6c37ff2db4287bed908e96e`。

## 当前：已授权完整移除 GCM 的独立本地发行副本

用户随后明确授权在**新副本**移除整个 GCM credential 组件；开发机原 366 文件 runtime
和旧源码 ZIP 均保持不动。新产物位于
`target/runtime-distribution/mingit-2.55.0.5-no-gcm-v2/`，未公开发布，也未批准整个 Neo release。
下方旧轮次的阻断与交付要求描述的是原快照，不应套用成新副本仍配送 GCM。

- 新 `runtime/gitbash/` 保留 **310 文件，67,457,243 B**。精确移除 50 个 GCM EXE/DLL、
  1 个 config、4 份 GCM 文档及 1 个 git-extra helper-selector 程序，共 **56 文件**；
  包括 `Microsoft.Identity.Client.NativeInterop.dll` 和 `msalruntime.dll`，不是仅删两个 DLL。
- Git、Bash、非 GCM 共享 DLL 及原许可保留；`git-credential-wincred.exe` 属于 Git，仍保留。
  selector 属于 git-extra，不误算为 GCM 包，因此 git-extra 对应源码仍保留。
- 只有 `etc/gitconfig`、`etc/package-versions.txt` 两个保留文件发生修改：前者去掉 GCM/
  selector helper 和两条机器 Git include，后者只去掉 GCM 包。完整移除、修改前后 SHA、
  原/新快照及验证结果在产物 `MANIFEST.json`，变更说明在 `MODIFICATIONS.md`。
- 新源码 companion 保留 **57 个保守包版本映射**：只从原 58 包中移除 GCM，其他即使无
  确切配送文件归属的 SDK 包不继续裁剪。整包排除含 GCM 二进制 payload 的
  `mingw-w64-git-credential-manager-2.9.1-1.src.tar.gz`，不修改旧历史 ZIP。
  其余原源码、补丁、构建/安装配方、固定 builder 的哈希不变。
- 新 ZIP 为 `gitbash-source-companion-no-gcm.zip`，**791,265,102 B、529 成员**，SHA-256：
  `999c56ee597ba1071d62e474522487b1b7727d530550966b16de1b99c331282a`。
  50 个 artifact 包含 48 个包源码归档、1 个 builder、1 个非二进制历史 notices supplement。
  新 `source-manifest.json`、`source-companion-record.json` 是匹配这个新运行时的记录。
  历史证据与取得配方仍可提及 GCM/58 包，不表示相应 payload 仍在新 ZIP 内，勿执行旧
  取得脚本来生成新发行包。

准备工具为 `tools/prepare_runtime_distribution.py`，固定输入与完整移除策略为
`tools/gitbash-distribution-policy.json`。工具仅接受匹配全部 366 文件 SHA 的原快照，
输出必须是显式可信根下的新目录；拒绝 symlink/junction、覆盖和源/目标重叠，完成 staging
校验后才 rename。保留文件原许可证随运行时原样复制；多余非二进制 notices 可以保留。

本机已通过 Bash 无 profile/rc 的 echo、Git 版本和隔离配置检查；135 个保留 PE 的
普通/delay imports 中未发现依赖已移除的本地 DLL。PE 静态检查不保证所有动态加载行为。
临时 HOME 和受控进程环境不读取开发机 global/system 配置，不请求网络或凭据、不运行 GUI。
新 ZIP 529 个成员哈希已独立读回复核。测试 15 项：14 通过，1 项因主机不能创建 symlink
跳过；Windows junction 拒绝测试通过。

**个人配置边界：**工具不修改任何用户 Git 配置。移除机器 include 可避免继承该安装的
helper，但发行 system config 无法禁止后来加载的个人 global helper；个人配置若指向
manager/manager-core/selector，需用户自行调整。隔离调用方可进程内控制
`GIT_CONFIG_GLOBAL`（私有空文件）与 `GIT_CONFIG_SYSTEM`，或单次
`git -c credential.helper=` 重置 helper 链。本轮不修改 Neo 应用行为。

最终打包须接入新运行时与新 companion，**不得继续配送开发机原缓存或旧 GCM ZIP**。
本轮未修改 workflow、发布检查或打包入口；父任务仍须核对最终 bundle、保留原许可，
并在同一 release 为匹配二进制/源码提供等效访问及真实稳定 URL。
`approval=false`、`release_ready=false`、`published=false` 不因本次技术移除自动转为 true。

## 以下为原 366 文件快照的历史记录

### 本地源码 companion 已齐备；尚未公开发布（历史）

- **58/58 包版本映射**：42 条精确 `.SRCINFO`，16 条经确切版本、动态包名模板与
  `mingw_arch` 验证的共享 MINGW64/UCRT 源码配方。不是拿同名 UCRT 二进制替代 MINGW64。
- **49 个包源码归档＋1 个固定 builder 归档，共 714,420,918 B**。包含实际源码、补丁、
  构建/安装配方及 git-extra 缺失的 `gitconfig`；GCC、lzip 和固定 Git 对象树已核对。
- 测试 fixture 原始字节保留、哈希可查；无需递归执行/展开恶意测试归档才算交付源码。
  同源重建须用相同 revision、补丁、配方和 MSYS/MINGW64 环境；不要求永远 bit-reproducible。
- 官方 HTTPS 取得的 release SHA 与本地计算 SHA 在清单中分层记录。
  **分离签名已保留但未验证**，不冒称独立签名认证。

本地文件（均未上传）：

| 文件（位于 `target/license-audit/gitbash/`） | 用途 |
|---|---|
| `MinGit-2.55.0.5-64-bit-source-companion-gcm-notices.zip` | 源码＋精确 GCM notices，804,515,487 B；本地阻断状态 |
| `source-manifest-gcm-notices.json` | 快照、58 包映射、源码/notices/README SHA 与精确阻断 |
| `source-reference.gcm-notices.local.json` | 小型版本/哈希/源码引用；URL 尚为空 |
| `gcm-notices/BUILD-AND-DELIVERY.md` | 重建、原通知、所有 runtime/source 变动需重审的边界 |

源码 ZIP SHA-256：
`86b9b45ad7390819448ddbdb37932340e3d2065a6613abfe7d05a01b0e91b943`。
旧 companion 保留为历史。新增 663,609 B notices supplement，不修改原 runtime/source。

### 原快照唯一尚未闭合的发布审查项（历史）

**已取得实际文件 notices；真正阻断是 NativeInterop 0.20.6 的再分发条款。**
50 个实际 EXE/DLL 已全部映射（6 个 GCM、44 个精确 NuGet），28 份原 notices/许可证
保存在 [`gcm/`](gcm/README.md)，包含同 release 完整 Avalonia、Skia/HarfBuzz 通知及
ANGLE 固定构建相关原许可。51 个程序/DLL/config 与官方 GCM payload 字节一致；
NuGet 比对仅排除严格界定的 Authenticode 签名字段，其余所有字节匹配。

精确包 `Microsoft.Identity.Client.NativeInterop 0.20.6` 的专有 LICENSE 第 **3(e)** 条限制
再分发，影响 `mingw64/bin/msalruntime.dll` 和
`mingw64/bin/Microsoft.Identity.Client.NativeInterop.dll`。原条款已保存，官方
[issue #6118](https://github.com/AzureAD/microsoft-authentication-library-for-dotnet/issues/6118)
及取得的回复未提供附加授权。不能用 MSAL/GCM 的 MIT 覆盖它。
必须取得适用授权，或另行明确授权更换/移除相关 runtime 后重审；本轮未擅自变更。
**源码归档内也含官方 GCM binary payload，因此当前 companion 同样不得直接公开上传。**

这不是要求 MIT 的 GCM 提供整个 NuGet/.NET 工具链源码或 bit-identical 重建。
本地清单明确区分 `corresponding_source_complete=true` 与
`release_ready=false`、`approval=false`、`published=false`。
未配送的 git-gui 程序不硬套源码义务；实际配送的 gitk 文档仍保留在范围内。

### 原含 GCM 快照的再分发方式（历史）

Neo 的许可证不替代组件许可证。解决上述确切再分发限制后，应将源码 companion 与匹配
二进制放在**同一 release 提供等效访问**，填入经确认的稳定 HTTPS URL；二进制随附
**整个 `docs/licenses/runtime/gcm/` 原通知目录**及小型源码引用，不反复内嵌巨大源码 ZIP。
只把 notices 放源码 ZIP 不等于随二进制交付。任意 runtime 文件或 source/notice/README
变动均须重算相关快照、清单、ZIP 与引用 SHA，当前结论仅适用这一版本。
不要把仍为空的本地 URL 说成已公开交付。

源码来源包括官方 [MSYS](https://repo.msys2.org/msys/sources/)、
[MinGW](https://repo.msys2.org/mingw/sources/)、Git for Windows 源资产，以及
[固定 build-extra revision](https://github.com/git-for-windows/build-extra/tree/cd940aab6443b0361cf113a06dbf2c0d9bf4cce4)。
第三方链接不替代再分发者自己的源码交付责任。
本文不是法律批准、发布签名或书面源码要约，不作“三年提供”承诺。
