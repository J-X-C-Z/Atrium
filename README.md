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

**对话**
- 流式 Markdown（代码高亮、复制），可折叠的思考过程（含用时）
- 等待首个输出时显示脉动的 "Thinking · Ns"，顶栏显示 "Working · Ns"
- 命令卡片（实时输出、耗时、退出码）；文件修改用真正的 diff 卡片（+/− 统计、逐行对比）
- 计划（`turn/plan/updated`）以步骤列表实时更新；MCP / 工具调用卡片
- 每轮结束后的页脚：复制整轮回答、重新生成、用时、Stopped / Failed 状态
- 失败的轮次显示错误卡片并可 "Try again"
- 内联审批：允许一次 / 本会话总是允许 / 拒绝；处理后收起为一行状态

**输入**
- 附件：图片（发给模型）与文件（作为 `@` 提及），支持选择或拖放，以 chip 显示、可移除
- `@` 文件提及：用 Codex 的 `fuzzyFileSearch` 在当前项目里模糊搜索
- `/` 命令：`/new` `/review` `/compact` `/attach` `/settings` `/sidebar`
- 访问模式（Read only / Auto / Full access，对应 Codex 的审批策略 + 沙箱）、推理强度（按模型支持的档位）、新对话的工作目录（含最近使用）

**对话管理**
- 侧边栏按日期分组、搜索、置顶；每条对话可重命名 / 置顶 / 归档；⌘B 折叠为图标栏（宽度动画）
- 顶栏：模型选择、标题、token 与上下文占比、对话菜单（重命名、置顶、代码审查、压缩上下文、归档）
- 代码审查（`review/start`）与压缩上下文（`thread/compact/start`）

**外观与操作**
- 新消息上浮淡入；历史加载时显示骨架屏；Toast 通知
- 命令面板 ⌘K：动作、对话跳转、访问模式、主题
- 设置 ⌘,：主题（跟随系统 / 浅色 / 深色）、默认访问模式、Codex 路径（保存后重启 Codex）
- 设置保存在 `~/Library/Application Support/AgentDesk/settings.json`

| 快捷键 | 作用 |
|---|---|
| ⌘N | 新对话 |
| ⌘K | 命令面板 |
| ⌘, | 设置 |
| ⌘B | 折叠 / 展开侧边栏 |
| ⌘L | 聚焦输入框 |
| ⌘⇧A | 添加附件 |
| ⌘. | 停止回答 |
| Enter / ⇧Enter | 发送 / 换行 |

mock 后端的触发词：消息里带 `run`（命令 + 审批）、`edit`（文件修改）、`plan`（计划）、`long`（长回答，方便测停止）、`fail`（失败的轮次）。

**macOS 启动时的 PATH**：从 Finder 启动拿不到 shell 的 `PATH`，所以启动时向登录 shell 询问 `PATH`，并搜索 Homebrew / npm / Volta 等常见位置；也可在设置或 `CODEX_BIN` 里指定。

## 结构

```
crates/
  agentdesk-core/   与后端无关的模型：ThreadMeta / Item / AgentEvent，以及把事件折叠成扁平行列表的 Transcript
  codex-client/     codex app-server 客户端：stdio JSON-RPC 传输、进程管理、协议 → AgentEvent 翻译；含 mock-codex
  agentdesk-app/    GPUI 应用：Workspace（状态与后端调用）+ ui/（sidebar、chat、composer、rows、调色板与设置）
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
- `codex-client`：JSON-RPC 请求/响应匹配、协议翻译，以及四条走真实子进程的端到端测试（带审批的完整一轮、中断、附件 + 计划 + 用时、重命名 + 压缩 + 审查 + 文件搜索）

## 下一步

- 后端抽象成 `AgentBackend` trait，接入 Claude Code（ACP / stream-json）
- 编排内核独立为 daemon，Hub MCP 实现跨后端派单与回执
- 场景（Scene）配置与网格面板
