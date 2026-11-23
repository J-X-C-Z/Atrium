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

装 Atrium 应用的电脑自带节点，只装节点的电脑单独装 `atrium-node`，开机自启。
节点的配置文件是 `~/.config/atrium-node/config.toml`，Windows 上是 `%APPDATA%\atrium-node\config.toml`：

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

## 需要在真机上确认的地方

第 18、19 项已经合并。下面几点只在模拟环境里测试过，配置完要在真机上确认：

- **远程 agent 能否用上项目工具**：Windows、Mac 上的 Codex 和 DeepSeek Harness 通过网络（Linux 节点的 `/v1/mcp`）使用项目工具。
  - 请确认它们能调用 `whoami`。如果调用失败，可能是 Codex 版本太旧、不支持通过 URL 接入 MCP 服务器，升级 Codex 后再试。
  - 也可能是 Linux 的节点只监听了本机地址，需要让它在 Tailscale 地址上监听。
- **跨电脑取文件**：在 Mac 上让 agent 用 `device_copy` 从 `win` 复制 `perf/core/`。注意单个文件不能超过 16 MB，一次复制总共不能超过 200 MB。
- **额度用完换替补**：只在打开自动驾驶（autopilot）时，会把任务交给替补重新开始。
- **节点令牌**：如果节点设了令牌，令牌会写进远程 agent 的 MCP 配置里，只在 tailnet 里使用。

遇到失败时，把报错原文告诉用户，不要绕过。
