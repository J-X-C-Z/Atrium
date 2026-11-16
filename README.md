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

**订阅（ChatGPT Plus / Pro）相关**

AgentDesk 以 `experimentalApi` 连接 Codex app-server，下面这些都走的是 Codex 自己的协议，用的就是你在 Codex 里登录的 ChatGPT 订阅，不需要另配 API key。

| 功能 | 做法 |
|---|---|
| **预测下一句** | 每轮结束后猜你接下来要说什么，显示为输入框里的灰色提示，按 Tab 采纳。先调 `thread/prediction/request`；Codex 0.169 回复 "not implemented"，于是退回到"以 `experimentalPredictionMode` fork 一个临时只读线程来猜"，猜完即 `thread/unsubscribe`。等 Codex 实现了原生预测会自动改用它。可在设置里关闭（每次预测会用掉一点额度） |
| 账号与登录 | 侧边栏显示邮箱与套餐徽章（Pro / Plus…）；应用内用 ChatGPT 登录（浏览器 OAuth）、登出 |
| 用量与限额 | 顶栏用量环；"账号与用量"里显示 5 小时与每周限额的已用比例与重置时间、credits、限额触顶提醒；每轮后实时更新 |
| Fast | 模型提供的 `priority` 服务档（约 2 倍速度，消耗更多额度） |
| Plan 模式 | `collaborationMode: plan`：先给出计划、等你确认再改动 |
| 长期目标 | 给对话设一个目标和 token 预算，Codex 跨多轮持续推进；顶部横幅显示进度，可暂停 / 继续 / 修改 / 清除 |
| 运行中插话（steer） | Codex 工作时继续发消息，会在它下一步读到（`turn/steer`） |
| 编辑并重发 | 改写已发出的消息，替换该轮及之后的内容（`thread/revert`，不会回滚磁盘上的文件） |
| 分支 | 从任意一轮复制出一个新对话（`thread/fork`） |
| 记忆 | 查看已记住的对话数；按对话开关记忆（`thread/memoryMode/set`） |
| Codex 提问 | 代理需要你做选择时显示问题卡片（选项 + 自由输入），回答后继续 |
| 生成图片、联网搜索、子代理 | 以图片、搜索卡片、子代理卡片显示 |
| 全文搜索 | 侧边栏搜索同时搜对话内容，"In messages" 下显示命中片段（`thread/search`） |

**没有做的**（以及原因）：实时语音（需要麦克风采集和 WebRTC 音频链路，是单独一块大工作）、远程控制配对、插件 / 应用市场、Projects、云端环境、Bedrock / GCP 登录、用户验证。它们在协议里存在，但要么跟桌面对话前端关系不大，要么需要单独设计界面，留到后续。

| 快捷键 | 作用 |
|---|---|
| ⌘N | 新对话 |
| ⌘K | 命令面板 |
| ⌘, | 设置 |
| ⌘B | 折叠 / 展开侧边栏 |
| ⌘L | 聚焦输入框 |
| ⌘⇧A | 添加附件 |
| ⌘. | 停止回答 |
| Tab | 采纳预测的下一句（输入框为空且有预测时） |
| Enter / ⇧Enter | 发送 / 换行 |

mock 后端的触发词：消息里带 `run`（命令 + 审批）、`edit`（文件修改）、`plan`（计划）、`long`（长回答，方便测停止和插话）、`fail`（失败的轮次）、`ask`（Codex 提问）、`image`（生成图片）、`search`（联网搜索）、`agents`（子代理）。mock 以 Pro 账号登录，并像 Codex 0.169 一样对 `thread/prediction/request` 回复 not implemented，从而走预测的 fork 路径。

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
- `codex-client`：JSON-RPC 请求/响应匹配、协议翻译，以及九条走真实子进程的端到端测试：带审批的完整一轮、中断、附件 + 计划 + 用时、重命名 + 压缩 + 审查 + 文件搜索、预测（fork 回退）、插话、编辑/分支/全文搜索、目标 + 账号 + 用量 + 登录登出 + 记忆、提问 + 图片 + Plan 模式 + Fast

## 下一步

- 后端抽象成 `AgentBackend` trait，接入 Claude Code（ACP / stream-json）
- 编排内核独立为 daemon，Hub MCP 实现跨后端派单与回执
- 场景（Scene）配置与网格面板
