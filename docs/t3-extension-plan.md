# Atrium 作为 T3 Code 的能力扩展层：改造方案（已暂停）

> **已暂停。** 改为把 T3 的核心移植到 Rust（`docs/native-parity-implementation.md`）。
> 本文和 `docs/t3-spike.md` 留作参考：移植的对比测试需要按 spike 的方式跑 T3。

日期 2026-11-23。依据：`docs/t3-spike.md`（T3 0.1.11 无界面驱动验证）。

## 一句话

日常对话、各家后端的接入、额度、排队、断线恢复、工作目录、PR 监控、定时任务都交给 T3。
Atrium 只做 T3 没有的部分：项目的多层分工（主控、组长、组员、任务文件、任务池、无人值守）、
团队（名单、替补、打转检测）、记忆、agent 群聊，以及一个看项目和编辑 Office 的小窗口。

## 结构

```
每台设备
  T3 服务（t3 serve）         ← 你在 T3 里聊天；Codex / Claude Code / Antigravity 由它启动
     ↑ MCP（t3_thread_launch / send / wait / read）
  atriumd（新，Rust，无界面） ← 项目调度、团队、记忆、群聊；对外提供 Atrium 的 MCP 工具
     ↑ 本地 HTTP
  Atrium 小窗口（GPUI）       ← 项目树、任务、群聊记录、团队、记忆设置、Office 编辑
```

- **atriumd**：把现在 `atrium-app` 里 GPUI 之外的调度逻辑（`workspace_project*.rs`、`workspace_team.rs`、
  通知和群组）搬进一个无界面的后台程序。它不再自己启动 Codex 或 Claude Code，而是通过 T3 的 MCP 开对话、
  发消息、等一轮结束、读结果。`.atrium/` 的文件格式不变。
- **一轮结束的信号**：T3 0.1.11 还没有事件订阅，先对每个运行中的对话挂一个 `t3_thread_wait` 长轮询；
  事件订阅进入正式版后再换。
- **Atrium 的 MCP 工具怎么进到 T3 的对话里**：T3 不能按对话加第三方 MCP。做法是 atriumd 在本机起一个 HTTP
  MCP 服务，然后：
  - Codex：在 T3 的 Codex 实例配置里加 `launchArgs = -c mcp_servers.atrium.url=...`（验证过参数能传到）；
  - Claude Code：写进项目的 `.mcp.json`（还没实测）。
  - 工具靠 T3 注入的 `T3CODE_THREAD_ID` 认出是哪个项目、哪个角色在调用（atriumd 开的对话它都记着 id）。
- **认证**：atriumd 用配对码走一次 T3 的 MCP 授权，拿到 30 天的令牌；到期前自动重新配对，失败时在小窗口提示。
- **跨设备**：每台设备各跑一个 T3。atriumd 通过 Tailscale 连各设备 T3 的 MCP，在哪台设备上开对话由项目的
  `spread` 规则决定（和现在一样，按 Atrium Workspace 相对路径）。`atrium-node` 只保留文件浏览和复制，
  对话转发部分删掉。

## 和 T3 重叠时谁说了算

- T3 会在每轮里提示 agent 优先用它的 `delegate_task`。在 Atrium 项目里，主控和组长的开场说明写清楚：
  项目内分工走 Atrium 的任务文件；临时的小查询可以用 `delegate_task`。
- 工作流：T3 的工作流还在实验阶段、默认关闭，且是 TS 代码文件。Atrium 的工作流（含数学建模模板、检查、
  关卡）先保留，跑在 atriumd 上，每一步通过 T3 开对话。等 T3 的工作流稳定后再评估是否迁过去。
- PR 监控、定时对话、Office 预览、额度显示、消息排队：用 T3 的，Atrium 这几块删掉。

## 删除

`atrium-app` 的聊天界面（对话列表、输入框、消息流、设备选择）、`codex-client` 和 `acp-client` 在 app 里的
使用、`atrium-sync` 的对话同步、PR 状态轮询、定时对话。`codex-client` 的 `mock-codex` 保留，用来测试
T3 驱动（需按 `docs/t3-spike.md` 补几个字段）。

## 分阶段

1. **T3 驱动**（新 crate `atrium-t3`）：MCP 客户端、配对授权和令牌续期、launch / send / wait / read、
   用 T3 + mock-codex 的端到端测试。补齐 mock-codex 让 T3 接受它。
2. **atriumd**：把项目调度、团队、通知、群组搬出 GPUI，换成 T3 驱动；HTTP MCP 服务；写 Codex 实例配置
   和项目 `.mcp.json`。端到端测试：一个主控 → 两个组长 → 组员任务，在 mock 上跑完。
3. **小窗口**：GPUI 只剩项目页、群聊记录、团队、记忆、Office 编辑；对话一律“在 T3 中打开”。删掉旧聊天代码。
4. **跨设备**：连多台设备的 T3；`atrium-node` 精简。
5. **记忆注入**：T3 没有会话开始的钩子，由 atriumd 在开对话时把记忆拼进第一条消息；在 T3 里你自己开的对话，
   靠 `AGENTS.md` / 技能提示 agent 去调 `recall`。

每阶段结束都能用：第 1、2 阶段期间，现在的 Atrium 照常可用。

## 风险

- **内存**：T3 服务空闲约 470 MB，每台设备常驻，另加后端进程。
- **版本变化快**：T3 还在 0.1.x。固定版本，升级前跑我们的端到端测试。
- **令牌 30 天**：没有续期接口，只能重新配对；如果配对需要人确认，每月要点一次。
- **T3 给每条消息加前缀、每轮加编排提示**：可能干扰 Atrium 给 agent 的分工说明，需要实测调整开场说明。
- **Claude Code 的 `.mcp.json` 注入、真 Codex / Claude 下的审批**都还没实测，第 1、2 阶段要在真机上确认。
