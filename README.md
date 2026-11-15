# AgentDesk

基于 Rust + [GPUI](https://www.gpui.rs) 的原生 Agent 桌面前端。第一阶段：类 ChatGPT 的对话界面，后端对接 **Codex CLI**（`codex app-server`）。

UI 组件：[Ely GPUI Components](https://github.com/ZacharyZhang-NY/Ely-GPUI-Components)（会话、消息、审批、工具卡片）+ [GPUI Kit](https://gpui-kit.com)（应用与窗口骨架）。

## 运行（macOS）

前置：Rust 1.95+；已安装并登录 Codex CLI（`npm i -g @openai/codex` 或 `brew install codex`，然后 `codex login`）。不需要完整 Xcode（GPUI 运行时编译 Metal shader）。

```bash
cargo run -p agentdesk-app
```

没有 Codex 账号也能开发 UI：用自带的 mock 后端（同一套协议、脚本化的回答；消息里带 "run" 会触发命令审批，带 "edit" 会产生文件修改）：

```bash
cargo build -p codex-client --bin mock-codex
CODEX_BIN=target/debug/mock-codex cargo run -p agentdesk-app
```

打包 `.app`：

```bash
script/bundle-macos.sh
```

## 已实现

- 侧边栏：新建对话（⌘N）、搜索、按日期分组的历史对话（来自 Codex 的 `thread/list`）、后端状态与一键重启；⌘B 折叠
- 对话：流式 Markdown（代码高亮）、可折叠的思考过程、命令执行卡片（实时输出、耗时、退出码）、文件修改卡片（diff）、MCP / 工具调用、计划步骤、错误与重试提示
- 审批：命令与文件修改的内联审批（允许一次 / 本会话总是允许 / 拒绝）
- 输入：多行输入，Enter 发送、Shift+Enter 换行；运行中变为停止按钮（⌘.）
- 顶栏：模型选择（`model/list`）、新对话的工作目录选择、token 用量、归档对话
- 跟随系统深浅色；设置保存在 `~/Library/Application Support/AgentDesk/settings.json`
- macOS 从 Finder 启动时拿不到 shell 的 `PATH`：启动时向登录 shell 询问 `PATH`，并搜索 Homebrew / npm / Volta 等常见位置；也可用 `CODEX_BIN` 或设置里的 `codex_binary` 指定

## 结构

```
crates/
  agentdesk-core/   与后端无关的模型：ThreadMeta / Item / AgentEvent，以及把事件折叠成扁平行列表的 Transcript
  codex-client/     codex app-server 客户端：stdio JSON-RPC 传输、进程管理、协议 → AgentEvent 翻译；含 mock-codex
  agentdesk-app/    GPUI 应用：Workspace（状态与后端调用）+ ui/（sidebar、chat、rows）
```

数据流只有一个方向：

```
codex app-server ──JSON-RPC──▶ codex-client::translate ──AgentEvent──▶ Transcript ──▶ UI
        ▲                                                                              │
        └──────────── turn/start · turn/interrupt · 审批应答 ◀──── Workspace ◀─────────┘
```

UI 只认识 `agentdesk-core` 的类型，不认识任何后端协议。接 Claude Code、DeepSeek Harness 或 ACP agent 时，只需要写一个新的 `xxx-client` 把它的协议翻译成 `AgentEvent`。Codex 协议标注为实验性、每个版本都会加字段，所以翻译层用 `serde_json::Value` 宽松读取：未知字段忽略，未知条目类型显示为普通系统行，不会让整条消息失败。

## 测试

```bash
cargo test --workspace
```

- `agentdesk-core`：Transcript 的归并规则（流式拼接、乐观消息去重、审批、失败与断连）
- `codex-client`：JSON-RPC 请求/响应匹配、协议翻译，以及两条走真实子进程的端到端测试（带审批的完整一轮、中断）

## 下一步

- 后端抽象成 `AgentBackend` trait，接入 Claude Code（ACP / stream-json）
- 编排内核独立为 daemon，Hub MCP 实现跨后端派单与回执
- 场景（Scene）配置与网格面板
