# 搭建跨电脑的 Atrium Workspace（给本地 agent 的配置说明）

这份说明写给帮用户配置环境的 agent。三台电脑按下表分工：

| 电脑 | 设备名 | 作用 | 要装的 agent |
| --- | --- | --- | --- |
| Linux | `linux` | 运行 Atrium（项目调度）、主控、界面设计 | Claude Code |
| Windows | `win` | 算法解析、性能优化的公共部分和 Windows 部分 | Codex、DeepSeek Harness（可选 Antigravity） |
| Mac | `mac` | 性能优化的 Mac 部分 | Codex、DeepSeek Harness |

每台电脑上的步骤都做完、最后的检查清单全部通过后，再告诉用户已经配好。
命令请按每台电脑的系统调整。遇到不确定的地方，先问用户，不要猜。

## 1. 网络

- 三台电脑都装 Tailscale，登录**同一个账号**，互相能 ping 通 Tailscale 地址。
- 不要把任何端口开放到 tailnet 以外。

## 2. Atrium Workspace 文件夹

每台电脑上建一个名为 `Atrium Workspace` 的文件夹。三台电脑上的名字必须完全相同，位置可以不同：

- Linux：`~/Atrium Workspace`
- Mac：`~/Atrium Workspace`
- Windows：`D:\Atrium Workspace`（按用户的磁盘决定）

## 3. atrium-node

装 Atrium 应用的电脑自带节点，只装节点的电脑单独装 `atrium-node`，开机自启
（注册用户服务的命令见第 8.3 节）。节点的配置文件是
`~/.config/atrium-node/config.toml`，Windows 上是 `%APPDATA%\atrium-node\config.toml`：

```toml
name = "win"                                # 依次用 linux / win / mac
roots = ['D:\Atrium Workspace']             # 必须包含这台电脑的 Atrium Workspace
```

Atrium 按 `roots` 里**名为 `Atrium Workspace` 的那一项**找到这台电脑的工作区。
如果 `roots` 不得不用别的名字，就在 Linux 上的 Atrium 里手动指定：在设置文件的 `device_workspaces` 里写上 `{"win": "D:\\..."}`。

## 4. agent

- **Linux**：装 Claude Code 和 `claude-agent-acp` 并登录。用户的 Claude 订阅只在这台电脑上用。
- **Windows、Mac**：
  - 装 Codex CLI，用 `codex login` 登录用户的 ChatGPT 账号；
  - 装 DeepSeek Harness（`dsh`），按它的文档填入用户的 DeepSeek API key。key 只写进这台电脑的配置，不要写进任何项目文件；
  - 用户需要的话，再装 Antigravity（`agy acp --login`）。
- 每台电脑都确认节点能找到这些 agent：在 Linux 的 Atrium 里打开设置 → 设备，每台设备下面应该列出它们。

## 5. Linux 上的 Atrium

1. 打开 Atrium，进入设置 → 设备：
   - "这台电脑"的 Atrium Workspace 应该是 `~/Atrium Workspace`；不是的话在这里改；
   - `win` 和 `mac` 都应该在线，并显示各自的 Atrium Workspace 路径。
2. 进入设置 → 团队，打开开关，确认成员。跨电脑的成员写在项目自己的团队文件里，见第 6 步。

## 6. 项目

假设项目名为 `photo-engine`，请改成用户实际的名字：

1. 在**三台电脑**的 Atrium Workspace 里都建 `photo-engine/`，并且只放这台电脑负责的子文件夹：
   - Linux：`ui/`，以及 Atrium 自动建的 `.atrium/`
   - Windows：`analysis/`、`perf/core/`、`perf/win/`
   - Mac：`perf/mac/`；需要公共代码时，用 `device_copy` 从 Windows 复制 `perf/core/`
2. 每个子文件夹在它所在的电脑上都可以有自己的 git 仓库，用来留历史。电脑之间不靠 git 同步。
3. 在 Linux 的 Atrium 里添加项目，根目录选 `~/Atrium Workspace/photo-engine`，主控选 Claude Code。
4. 写 `photo-engine/.atrium/team.toml`，把各台电脑上的 agent 写成不同成员：

   ```toml
   on = true

   [members.claude]
   agent = "claude"
   notes = "主控和界面设计。额度最少，留给规划和界面。"
   stand_ins = ["gpt-win"]

   [members.gpt-win]
   agent = "codex@win"
   notes = "Windows 上：算法解析、性能优化公共部分和 Windows 部分。严谨细致。"
   stand_ins = ["deepseek-win"]

   [members.gpt-mac]
   agent = "codex@mac"
   notes = "Mac 上：性能优化的 Mac 部分，跑 Mac 基准测试。"
   stand_ins = ["deepseek-mac"]

   [members.deepseek-win]
   agent = "deepseek@win"
   notes = "Windows 上，几乎不限量但容易打转：只给能用命令验收的活，比如批量导出样张、计算色差。"

   [members.deepseek-mac]
   agent = "deepseek@mac"
   notes = "Mac 上，同上。"
   ```

5. 写 `photo-engine/.atrium/brief.md`，写清楚项目目标、三个板块各自的文件夹，以及"每个文件夹只由一台电脑写"这条规则。具体内容向用户确认后再写。
6. 负责人由主控创建，不需要手动建。主控会在任务文件里写 `agent:` 和 `scope:`，例如：

   ```markdown
   # 算法解析
   agent: codex@win
   scope: analysis/**

   用黑盒对比的方法分析……（目标由用户和主控商定）
   ```

## 7. 检查清单

- [ ] 三台电脑在同一个 tailnet 里，Linux 上的 Atrium 在设置 → 设备里看到 `win` 和 `mac` 都在线。
- [ ] 每台设备都显示了正确的 Atrium Workspace 路径。
- [ ] Windows 和 Mac 上的 Codex、DeepSeek Harness 都显示可用。
- [ ] 在 Linux 的 Atrium 里和 `Codex · win` 开一个普通对话，让它运行 `cd` 或 `pwd`：能在 Windows 上执行。
- [ ] 项目里给 `codex@win` 建一个试验任务，`scope: analysis/**`。它启动后的工作目录应该是 `D:\Atrium Workspace\photo-engine`。

## 8. 安装包、服务与自动更新

### 8.1 下载哪个文件

从 GitHub Releases（`https://github.com/J-X-C-Z/Atrium/releases`）下载与系统和
CPU 对应的文件：

| 系统 | 首选 | 说明 |
| --- | --- | --- |
| macOS Apple Silicon | `Atrium-<版本>-macos-arm64.dmg` | 拖进“应用程序” |
| macOS Intel | `Atrium-<版本>-macos-x86_64.dmg` | 同上 |
| Windows x64 | `Atrium-<版本>-windows-x64-setup.exe` | 当前用户安装，不需要管理员 |
| Windows x64（便携） | `Atrium-windows-x64.zip` | 解压后直接运行 |
| Linux x86_64 | `Atrium-<版本>-linux-x86_64.AppImage` | 需要桌面环境 |
| Linux x86_64（Debian/Ubuntu） | `Atrium-<版本>-linux-x86_64.deb` | `apt install ./<文件>` |
| 只装节点 | `atrium-node-<版本>-<系统>-<架构>.tar.gz`（Windows 为 `.zip`） | 内含 `atrium-node` 和 `manifest.json` |

同一发布页还有 `SHA256SUMS`。下载后逐个核对，例如
`shasum -a 256 Atrium-*.dmg`（macOS）或 `sha256sum Atrium-*.AppImage`
（Linux），结果应与 `SHA256SUMS` 中对应行一致。Windows 可用
`Get-FileHash -Algorithm SHA256 <文件>`。

### 8.2 安装 Atrium 应用

- **macOS**：打开 `.dmg`，把 `Atrium.app` 拖到“应用程序”。维护者配置了
  签名和公证的发布包可以直接打开；没有签名密钥时 CI 会生成 ad-hoc 签名
  的包，首次启动需要在 Finder 里右键选择“打开”，或由维护者重新签名。
- **Windows**：运行 `setup.exe`。安装器按当前用户安装到
  `%LOCALAPPDATA%\Programs\Atrium`，可选桌面快捷方式，卸载走“设置 →
  应用”或开始菜单里的卸载项。Windows 安装包目前没有代码签名，
  SmartScreen 提示时选择“更多信息 → 仍要运行”。
- **Linux**：AppImage 先 `chmod +x` 再运行；`.deb` 用
  `sudo apt install ./Atrium-<版本>-linux-x86_64.deb`。AppImage 与 deb
  都依赖系统的图形库，建议在较新的发行版（glibc 2.35 及以上）使用。

### 8.3 把 `atrium-node` 装成用户服务

先按第 3 节写好并确认 `config.toml`。`install` 会先校验配置，再写入服务
定义并启动；如果已经有同名服务，它会拒绝覆盖。配置文件的实际位置可用
`atrium-node config path` 查看。

**macOS（launchd，用户级 LaunchAgent）**

```sh
NODE="/Applications/Atrium.app/Contents/Resources/bin/atrium-node"
"$NODE" install  --config "$HOME/.config/atrium-node/config.toml"
"$NODE" status
"$NODE" uninstall
```

服务定义在 `~/Library/LaunchAgents/dev.atrium.node.plist`，日志在
`~/Library/Logs/atrium-node.log`。也可以解压独立的 node 包，用包里的
`atrium-node` 执行同样的命令。

**Linux（systemd 用户单元）**

```sh
atrium-node install  --config "$HOME/.config/atrium-node/config.toml"
atrium-node status
atrium-node uninstall
# 需要在注销后继续运行：
loginctl enable-linger "$USER"
```

服务定义在 `~/.config/systemd/user/atrium-node.service`，卸载会停止并删除
该单元，然后执行 `systemctl --user daemon-reload`。

**Windows（当前用户计划任务）**

```powershell
$node = "$env:LOCALAPPDATA\Programs\Atrium\atrium-node.exe"
& $node install  --config "$env:APPDATA\atrium-node\config.toml"
& $node status
& $node uninstall
```

Windows 上注册的是登录时启动的**当前用户计划任务**
（`AtriumNode-<SID>`），不是系统服务。这样 agent 才能使用当前用户的
家目录、钥匙串和登录状态；这也是 `docs/node-implementation.md` 给出的
两种方案之一。

### 8.4 自动更新

Atrium 启动约 20 秒后、之后每天检查一次 GitHub 的最新 Release：

1. 比较 Release 的 tag 与本机版本；
2. 在 Release 的附件里按当前系统与架构选择安装包（macOS 选 `.dmg`，
   Windows 优先 `setup.exe`、其次 `.msi`，Linux 优先 `.AppImage`、其次
   `.deb`）；
3. 侧栏显示“Atrium <版本> 已发布 / 下载”，点击直接打开对应安装包。

找不到匹配附件时退回到 Release 网页。Atrium 不会静默下载或替换自身：
退出旧版本后运行新安装包即可，用户数据和 `Atrium Workspace` 不受影响。

### 8.5 发布流程（维护者）

1. 修改 `Cargo.toml` 里的 workspace 版本并提交；
2. 打与版本完全一致的 tag（`v<版本>`，例如 `v0.2.0`）并推送；
3. `.github/workflows/release.yml` 会先校验 tag，再构建并上传：

   - macOS arm64 与 x86_64：`.dmg`、`.zip`，以及 `atrium-node` 的 `.tar.gz`；
   - Windows x64：安装器 `.exe`、便携 `.zip`，以及 `atrium-node` 的 `.zip`；
   - Linux x86_64：`.AppImage`、`.deb`，以及 `atrium-node` 的 `.tar.gz`；
   - 全部附件下载后生成 `SHA256SUMS`。

macOS 签名/公证使用以下仓库 Secrets；**全部不配置时构建照常完成**，
脚本会打印跳过签名和公证的说明，并生成 ad-hoc 签名的 `.dmg`：

| Secret | 内容 |
| --- | --- |
| `APPLE_CERTIFICATE_P12` | Developer ID Application 证书的 base64 编码 `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | 导出 `.p12` 时设置的密码 |
| `APPLE_SIGNING_IDENTITY` | 例如 `Developer ID Application: Name (TEAMID)` |
| `APPLE_ID` | Apple 账号邮箱 |
| `APPLE_TEAM_ID` | Team ID |
| `APPLE_APP_PASSWORD` | Apple ID 的 app 专用密码 |

只有身份和三项公证凭据都存在时才提交公证；缺任何一项都会跳过公证。
任何密钥、密码和证书都只放在 GitHub Secrets 里，不能写进仓库或日志。

Linux 的 appimagetool 由工作流下载。可选的仓库变量
`APPIMAGETOOL_SHA256` 用于校验下载文件的 SHA-256；未设置时工作流会给出
警告并继续。

在本机手工构建：

```sh
./script/bundle-macos.sh --dmg        # macOS
./script/bundle-windows.ps1 -Installer  # x64 Windows，需 Inno Setup 6
APPIMAGETOOL=/path/to/appimagetool ./script/bundle-linux.sh  # Linux
```

三个脚本都支持 `--check`（Windows 为 `-Check`），只打印工具链和签名状态，
不执行构建。产物都在 `dist/`。

## 需要在真机上确认的地方

第 18、19 项已经合并。下面几点只在模拟环境里测试过，配置完要在真机上确认：

- **远程 agent 能否用上项目工具**：Windows、Mac 上的 Codex 和 DeepSeek Harness 通过网络（Linux 节点的 `/v1/mcp`）使用项目工具。
  - 请确认它们能调用 `whoami`。如果调用失败，可能是 Codex 版本太旧、不支持通过 URL 接入 MCP 服务器，升级 Codex 后再试。
  - 也可能是 Linux 的节点只监听了本机地址，需要让它在 Tailscale 地址上监听。
- **跨电脑取文件**：在 Mac 上让 agent 用 `device_copy` 从 `win` 复制 `perf/core/`。注意单个文件不能超过 16 MB，一次复制总共不能超过 200 MB。
- **额度用完换替补**：只在打开自动驾驶（autopilot）时，会把任务交给替补重新开始。
- **节点令牌**：如果节点设了令牌，令牌会写进远程 agent 的 MCP 配置里，只在 tailnet 里使用。
- **安装包与服务**：macOS `.dmg`、Windows 安装器和 Linux AppImage/`.deb`
  要在三台真机上各安装一次；macOS 的签名和公证只有在配置 Secrets 后才
  会真正执行。每台机器执行一次 `atrium-node install`，重启后确认服务自动
  启动，再执行 `atrium-node uninstall` 确认能清理干净。Windows 上确认注册
  的是当前用户的计划任务。

遇到失败时，把报错原文告诉用户，不要绕过。
