# Orca 实机验证记录（Orca 1.4.273 / Windows）

验证环境：Windows 11，Orca Desktop `1.4.273`（Plugin API v1，`trusted-worker`），Node.js `v22.23.2`。

## 1. 关键机制核查与修复

### 1.1 `agent.launch.before` hook 能否匹配到终端？
- **结论**：**可以**。
- **机制与实测**：
  - `orca-host.d.ts` 中 `"agent.launch.before"` 的事件结构为 `{ agent, workspaceId, workspaceKind, terminalId?, command, env }`。
  - `worker/src/bridge/hook.ts` 的 `LaunchHook.match` 通过 `(workspaceId, agent)`（当 `workspaceId` 为 `null` 时回退到 `agent` 类型）匹配 `LaunchRequest` 发起前登记的 `pending` 项，并在 `event.terminalId` 存在时回填 `ours.terminalId`。
  - 通过 `orca plugin dev hook local.atrium agent.launch.before` 与真实 `run.start`（触发 `agent.launch` 启动 `term_79f0ea91-8d71-481e-b58b-63b8b920c7a4`）验证，hook 正常匹配并注入环境，未出现未匹配告警。

### 1.2 `atrium` 命令能否在 agent 终端里执行？
- **结论**：**修复后可以**。
- **发现的问题**：在 Windows 的 Orca 终端（PowerShell）中，默认 `PATH` 可能不包含用户单独安装的 `node.exe`（例如 `node` 位于 `%LOCALAPPDATA%\hermes\node` 而非系统 `PATH`），导致 `bin/atrium.cmd` 直接调用 `node "%~dp0atrium" %*` 时报 `'node' 不是内部或外部命令`。
- **修复**：
  - 在 `worker/src/bridge/hook.ts` 中注入 `ATRIUM_NODE: process.execPath`。
  - 在 `bin/atrium.cmd` 中优先使用 `ATRIUM_NODE` 并设置 `ELECTRON_RUN_AS_NODE=1`（兼容 Electron 宿主的 `Orca.exe` 与独立 `node.exe`），未设置时回退到 `node`。

### 1.3 `ORCA_TERMINAL_HANDLE` 与状态事件里的 `terminalId` 是否一致？
- **结论**：**一致**。
- **实测**：通过 `orca terminal create` 创建终端返回的 `terminal.handle` 为 `term_e73c2e34-ad99-4dfd-93e4-4d91bf4f9045`，在该终端内读取 `$env:ORCA_TERMINAL_HANDLE` 同样为 `term_e73c2e34-ad99-4dfd-93e4-4d91bf4f9045`。

### 1.4 Worker 是否有 `orca.settings.onDidChange`？
- **结论**：**有**。
- **核查**：`orca-worker.d.ts` 中 `OrcaWorkerApi.settings` 为 `SettingsApi`，提供 `onDidChange`（会监听用户在 Orca 设置界面的修改）；`worker/src/host.ts` 第 61 行已通过 `orca.settings?.onDidChange?.(() => listener())` 接入。

### 1.5 Agent 终端能否连上 socket？
- **结论**：**可以**。
- **实测**：在 Windows 上 `worker/src/bridge/server.ts` 使用命名管道 `\\.\pipe\atrium-orca-<hash>`。在 Orca 创建的真实终端中设置 `ATRIUM_SOCKET` 并执行 `bin/atrium status`，成功连上 worker 的 `BridgeServer` 并收到 JSON 响应（无 token 时返回 `atrium: this terminal was not started by Atrium (bad token)`）。
- **测试修复**：`test/helpers/setup.mjs` 原先固定使用 `join(dir, 'a.sock')`，在 Windows 下 `net.Server.listen` 报 `EACCES`，已改为使用 `\\.\pipe\atrium-test-<hash>`。

### 1.6 `editor.open` 能否打开 `.atrium/` 下的文件？
- **结论**：**可以**。
- **实测**：分别调用 `board.open`（写入并打开 `<project>/.atrium/atrium.board`）和 `run.open`（写入并打开 `<run>/.atrium/view.atrium-run`），并使用 `orca plugin shot local.atrium --editor ...` 实机截图，均返回 `settled: true, subscribed: true, errors: []`。

### 1.7 WASM 能否在 Orca 的 worker 里加载？
- **结论**：**修复 Windows WASI 兼容问题后可以**。
- **发现的问题**：
  1. `worker/src/core.ts` 原默认 `ROOT = { '/': '/' }`，在 Windows 上 `wasi-libc` 不识别 `C:\...` 盘符路径（`os error 28: Invalid argument`）。
  2. Node.js `node:wasi` 在 Windows 上对 `fd_readdir` 返回 `ENOSYS (52)`，导致 `workflow.templates` 与 `read_tasks` 的 `fs::read_dir` 全部返回空列表。
- **修复**：
  1. 在 `worker/src/core.ts` 中，Windows 下将各盘符根目录映射为 `/c`、`/d` 等 POSIX 挂载点，并在 `Core.run` 入参/出参间双向转换 `C:\...` ↔ `/c/...`。
  2. 在 `worker/src/core.ts` 中添加 `patchWinReaddir`，跟踪 `fd_prestat_dir_name` / `path_open` / `fd_close` 打开的目录 fd，当原生 `fd_readdir` 返回 `52` 时通过 `fs.readdirSync` 构造 WASI `dirent` 缓冲返回。
- **实测**：加载插件后 `orca plugin logs local.atrium` 正常输出 `info atrium: ready`，所有项目/团队/工作流 core ops 在真机 Orca worker 中运行正常。

### 1.8 `agent.launch` 是否按角色的工作目录启动？
- **结论**：**不支持指定 `cwd`**。
- **核查**：`orca-host.d.ts` 中 `"agent.launch"` 的参数签名为 `{ workspaceId: string; agent: string; prompt?: string }`，不包含工作目录参数；agent 总是在 `workspaceId` 对应的工作区根目录（或通过 `workspace.create` 创建的 worktree 目录）中启动。角色配置中的子目录（`folders`）通过提示词/kickoff 文本传达，而非由 `agent.launch` 切换进程 `cwd`。

---

## 2. 实机功能检查与截图

通过 `orca plugin dev load . --approve ap1-bc93c158863d1552e7029cbbc3fe8294` 加载并批准后，完成以下实机验证与 `orca plugin shot` 截图：

- **侧边栏（Sidebar）**：`live-sidebar.png`、`live-sidebar-active.png`（`349×838`，`settled: true`）。
- **状态栏（Status Bar）**：`live-status-bar.png`（`1661×24`，显示 `0 项需要你 · 0 个运行中`）。
- **项目看板（Board）**：`project.init` → `board.open` → `team.save` → `live-board.png`（`1200×800`，`settled: true, subscribed: true`）；任务拖拽流转与自动驾驶/替补切换由自动化集成测试（`test/project-autopilot.test.mjs`、`test/real-core.test.mjs`）及核心单测覆盖通过。
- **工作流运行（Run）**：`run.start` → 启动 agent 终端 → `run.open` → `live-run.png`（`1200×800`，`settled: true, subscribed: true`）；关口自定义选项、通过与退回逻辑由 `cargo test -p atrium-orca-core` 与 `npm test` 全链路覆盖通过。
- **重启恢复（Restart）**：执行 `orca plugin restart local.atrium` 后再次调用 `overview.get` 与 `run.get`，运行状态与步骤记录从 `.atrium/run.json` 完整恢复。
