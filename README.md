# Atrium

> 每个会话是一个独立的房间，各自朝长期目标推进；中庭是它们共享、对齐信息的地方。

基于 Rust + [GPUI](https://www.gpui.rs) 的原生 Agent 桌面前端：类 ChatGPT 的对话界面，后端对接 **Codex CLI**（`codex app-server`），以及通过 Agent Client Protocol（ACP）对接的 **Claude Code**、**DeepSeek Harness**、**Antigravity**。

UI 组件：[Ely GPUI Components](https://github.com/ZacharyZhang-NY/Ely-GPUI-Components)（会话、消息、审批、工具卡片）+ [GPUI Kit](https://gpui-kit.com)（应用与窗口骨架）。

## 运行（macOS）

前置：Rust 1.95+；已安装并登录 Codex CLI（`npm i -g @openai/codex` 或 `brew install codex`，然后 `codex login`）。不需要完整 Xcode（GPUI 运行时编译 Metal shader）。

```bash
cargo run -p atrium-app
```

没有 Codex 账号也能开发 UI：用自带的 mock 后端（同一套协议、脚本化的回答；消息里带 "run" 会触发命令审批，带 "edit" 会产生文件修改）：

```bash
cargo build -p codex-client --bin mock-codex
CODEX_BIN=target/debug/mock-codex cargo run -p atrium-app
```

打包 `.app`：

```bash
script/bundle-macos.sh
```

## 两种用法：聊天与项目

侧边栏顶部切换"Chats / Projects"。

**聊天**：日常问答，就是下面描述的对话界面，选一个 agent 就开始。

**项目**：一个代码目录，由几条**工作线**长期并行推进，可以用主控 → 负责人 → 子 agent 的三层结构组织（见下文）。每条工作线是一个独立会话（任意 agent），有自己的长期目标和负责范围，自己往前走，不需要总管反复督促；工作线之间通过项目目录里的**中庭**（`.atrium/`）对齐：

```
.atrium/project.json      工作线（Atrium 自己的记录）
.atrium/brief.md          共识：架构、约定、决定。所有工作线开工先读
.atrium/contracts/*.md    工作线之间的接口（API、数据结构）。改接口的一方更新文件并发公告
.atrium/status/<id>.md    每条工作线自己的状态：Focus / Done / Next / Touching / Questions
.atrium/notices.md        只追加的公告："- 时间 · 发送方 → 接收方: 内容"
```

都是普通文件，agent 用自己的读写文件能力就能参与，不依赖任何专门协议；可以提交进 git。

- **新建工作线**：名字、agent、长期目标、负责范围（如 `web/**`）。Atrium 新建会话，发一段开场说明（目标、范围、其他工作线、中庭的规则），然后它自己开工。Codex 另外设置原生长期目标（`thread/goal`），跨轮次自动推进；其他 agent 没有这个能力，空闲时点"Carry on"继续
- **概览**：每条工作线一张卡片：状态（工作中 / 需要你 / 空闲 / 出错 / agent 未运行）、目标、范围、计划进度与当前步骤、状态文件里的 Focus、最近说的话、未读公告、越界次数、速度和上下文占用
- **需要你**：所有工作线等你审批或回答的事项汇总在概览顶部，侧边栏项目旁有角标
- **公告只推增量**：发给某条工作线（或所有人）的新公告，在它下一条消息前自动附上；也可以点"Hand over notices"立即移交（Codex 运行中走插话，读到下一步）。不会把整个中庭塞进上下文
- **越界提醒**：Atrium 看得到每个会话改了哪些文件。改到别人范围内的文件时，卡片标出，弹出提示，并自动发公告给范围所属的工作线
- **中庭面板**：公告（可以自己发，选择接收方）、Brief、Contracts、各工作线状态；文件变化 2 秒内刷新，"Edit" 用系统编辑器打开
- 工作线的会话不出现在普通聊天列表里；点进去就是完整的聊天界面，上方一条项目导航（返回、目标、范围、移交公告、移出项目）

### 三层结构：主控 → 负责人 → 子 agent

层级固定为三层，不再往下嵌套：

- **主控**（每个项目最多一个）：你给它项目总目标。它不写代码，负责拆分：在 `.atrium/plan.md` 里写里程碑和归属，为每个负责人写一份任务文件（目标、范围、`## Roadmap` 路线图）。之后它只在两种情况下被唤醒：负责人在里程碑处发公告给它，或者某个负责人交付了。平时不轮询，不占上下文
- **负责人**：长期负责一块（如前端），按主控给的路线图逐阶段推进，每完成一步就在任务文件里把 `- [ ]` 改成 `- [x]`，每完成一个阶段给主控发公告
- **子 agent**：负责人把独立的小块写成 `.atrium/tasks/<负责人id>/<任务>.md`，Atrium 为每个文件启动一个子 agent（每位负责人同时最多 3 个，其余排队）。子 agent 做完在文件里写 `## Result` 并把 `status:` 改成 `done`，负责人随即收到通知

```
.atrium/plan.md                   主控的计划
.atrium/tasks/<owner>/<id>.md     请求下一层的 agent：主控 → 负责人，负责人 → 子 agent
```

主控写好的路线图直接交给对应的负责人，不需要你中转。想先过目的话，可以在项目菜单里打开"主控提议的负责人需经我批准才启动"，之后概览里会出现"主控提议了 N 位负责人"，逐个"开始 / 驳回"。你也可以不用主控，直接"新建负责人"。

**进度视图**（概览默认）：每个负责人一行，路线图按阶段（`### 阶段名`）排成一串方块。做完的阶段变暗，当前阶段高亮，并显示步骤进度、下一步和正在跑的子 agent，后面的阶段正常显示。一眼就能看出谁做到了哪里。也可以切回卡片视图。

**agent 怎么调用这些能力：`atrium` MCP 服务 + skill**

- **MCP 服务**：Atrium 启动或恢复项目里的会话时，会自动给它挂上一个名为 `atrium` 的 MCP 服务（就是 Atrium 自己：`atrium mcp --project <目录> --stream <id>`）。Codex 通过 `thread/start` / `thread/resume` 的 config 注入，Claude Code、DeepSeek、Antigravity 通过 ACP 的 `mcpServers` 注入。服务启动时已经绑定了这个 agent 的身份，它没法冒充别的工作线；工具清单也按角色给（子 agent 看不到 `create_task`，只有主控有 `write_plan`）。

  | 工具 | 用途 |
  | --- | --- |
  | `whoami` | 我是谁：角色、范围、目标、上级、路线图和当前阶段、我派出的任务 |
  | `create_task` | 主控派负责人（必须带分阶段路线图）；负责人派子 agent |
  | `check_step` | 负责人勾掉一步（`2.1` 或步骤文字）；一个阶段做完自动通知上级 |
  | `finish_task` | 交付或失败，写 `## Result` 并改状态 |
  | `notify` / `read_notices` | 发公告（id、`all` 或 `you`）/ 看未读公告 |
  | `write_status` / `write_plan` | 写自己的状态文件 / 主控写计划 |
  | `list_tasks` | 我的任务，或全部任务 |

  它只改 agent 本来也能手改的文件（任务文件、`plan.md`、`status/`、`notices.md`），从不写 `project.json`，所以和调度引擎、手工编辑完全兼容。已用真实的 Claude Code 验证过：主控通过它建出分阶段的负责人任务，负责人勾步骤、阶段完成自动通知主控、交结果。
- **Skill**：[`skills/atrium/SKILL.md`](skills/atrium/SKILL.md) 讲清三种角色各自怎么做、什么时候用哪个工具，以及没有工具时直接改文件的格式。`atrium skill install` 会把它装到 `~/.claude/skills/atrium/` 和 `~/.codex/skills/atrium/`；`atrium skill` 直接打印。没装 skill 也能用：每个会话的开场说明和 MCP 服务的 instructions 里都有要点。

**任务池：子 agent 不停地干活**（参考 [Paperclip](https://github.com/paperclipai/paperclip) 的 checkout 机制）

负责人不再一个一个地派子 agent，而是用 `create_tasks` 一次往自己的任务池里放一批任务（可以写 `after:` 依赖和 `priority:` 优先级）。Atrium 为它启动几个常驻的子 agent（最多 `max_workers` 个），每个子 agent 领一个任务、做完交结果、`finish_task` 当场帮它领下一个，一直做到池子空。领任务是原子的（跨进程文件锁），同一个任务不会被两个人领走；已经领了的再领会拿回同一个。只有当待领任务少于子 agent 数、或者有任务失败时，才叫醒负责人：附上这期间所有结果的摘要，让它审阅、勾路线图、补下一批。子 agent 做满若干个任务后会换一个新会话，避免上下文越来越乱。概览里每个负责人的当前阶段下会显示任务池（待领 · 进行中 · 已完成）和每个子 agent 手上的任务。

**无人值守：24 小时持续运行**

项目菜单里打开"无人值守，24 小时持续运行"后，概览标题栏会出现一个绿色的"无人值守"标记，所有 agent 的开场说明里都会写明"没人回答问题，不要停下来问，自己做合理决定并记录"。调度上借鉴 Paperclip 的心跳和存活恢复：还有活没干完的 agent 一轮结束就被续上；空闲的负责人和主控定期心跳检查；一轮长时间没输出就打断重来；失败按退避重试（最长每 30 分钟一次，不放弃）；Atrium 重启后自动恢复所有没做完的会话；项目目录内的审批自动通过、目录外的拒绝并通知；可设每日轮数上限；运行期间阻止电脑休眠。真正 24 小时不停，建议把 agent 放在一台常开的机器上（`agent@设备`，通过 atrium-node）。

调度器按任务文件启动 agent、汇报结果并唤醒上级。公告投递状态不确定时，打开对应工作线，选择「处理公告投递」来确认已收到或仅重试公告。
任务池与无人值守调度的后续要求见 [实现说明](docs/autopilot-implementation.md)。

当前限制：工作线共用一个代码目录（还没有每条线独立 worktree）；越界提醒只针对 agent 报告的文件修改，不包括它在命令里直接写的文件。

## 多设备：tailnet 里其他电脑上的 agent

在别的电脑上运行 `atrium-node`（后端，按 [`docs/node-protocol.md`](docs/node-protocol.md) 和 [`docs/node-implementation.md`](docs/node-implementation.md) 实现），它们的 agent 就出现在 Atrium 里：

`atrium-node` 可在 macOS、Linux 和 Windows 上独立运行。首次使用先运行 `atrium-node config path`，创建配置并指定允许访问的 `roots`，再运行 `atrium-node serve --config <配置文件>`。节点可通过 `install`、`status` 和 `uninstall` 交给当前用户的 launchd、systemd 或 Windows 任务计划程序管理。[构建、打包与验收状态](docs/backend-acceptance-matrix.md#cross-platform-follow-up)

- **发现**：读本机 `tailscale status`，探测每台在线设备的 7381 端口；也可以在设置 → Devices 里按地址添加（`ATRIUM_NODES=host:port` 同样可以）
- **选择**：顶栏的 agent 选择器按设备分组（"This computer"、"studio"……）；远端会话在侧边栏标为 `Claude@studio`、`Codex@studio`
- **文件夹**：远端新对话的文件夹从那台设备的根目录里逐级选择（通过节点列目录）
- **原样转接**：节点只负责启动 agent 并把它的标准输入输出转到 WebSocket，Codex 和 ACP 协议端到端不变，所以审批、计划、diff、用量、速度在远端会话里都一样可用
- **断线不丢**：笔记本合盖、换网络时，进程在那台设备上继续跑；Atrium 自动重连，并按序号补回错过的输出，断线期间写出的消息等连上再发。顶栏显示"Reconnecting to studio…"
- 远端 Codex 用它自己的模型设置（本机的模型列表、Fast、Plan、推理强度对它不生效）；账号与用量只显示本机 Codex 的

当前限制：项目（工作线 + 中庭）只支持本机文件夹；远端项目需要通过节点读写 `.atrium/`，协议里已经定义（`/v1/fs/*`），界面还没接。Atrium 重启后不会接回之前在远端启动的进程，而是新起一个（agent 自己保存的会话照常可以打开）。

## 支持的 Agent

| Agent | 接入方式 | 本机需要 |
|---|---|---|
| Codex | `codex app-server`（JSON-RPC） | `codex`（`npm i -g @openai/codex` 或 `brew install codex`） |
| Claude Code | 官方 ACP 适配器 `claude-agent-acp` | `claude-agent-acp`（`npm i -g @agentclientprotocol/claude-agent-acp`）；只装了 `claude` 时用 `npx` 自动获取适配器 |
| DeepSeek Harness | `dsh --profile acp` | `dsh`（`npm i -g @deepseek-ai/dsh`） |
| Antigravity | `agy acp`（agy 1.5+） | Antigravity CLI，首次运行 `agy acp --login` |

启动时自动在 `PATH`（以及登录 shell 的 `PATH`）里查找；找不到的 agent 在选择器里显示为未安装，设置里给出安装命令。可以按 agent 覆盖启动命令：设置文件里的 `agent_commands`（如 `{"deepseek": "/opt/dsh --profile acp"}`），或环境变量 `ATRIUM_CLAUDE_ACP` / `ATRIUM_DEEPSEEK_ACP` / `ATRIUM_ANTIGRAVITY_ACP`。

- **新对话**：顶栏选 agent（记住上次的选择）。选中后会提前建好一个会话，于是输入框立即显示该 agent 自己的设置（模式、模型、推理强度……，其余收在 "Options" 里），第一条消息也更快。
- **对话中**：流式回答、思考过程、工具调用（命令、文件读写、diff）、计划、权限审批、token 用量、该 agent 的斜杠命令，与 Codex 用同一套界面。
- **历史**：侧边栏合并显示所有 agent 的会话（标注来源）。能回放历史的 agent（Claude、Antigravity）打开时重放；只能恢复的（DeepSeek）继续会话并注明没有历史。
- **Codex 专属**：预测下一句、长期目标、记忆、编辑重发、分支、代码审查、压缩、全文搜索、账号与用量。这些在其他 agent 的对话里不显示。
- Atrium 不向 agent 提供文件系统和终端能力：读写文件、运行命令都由 agent 自己完成，需要时通过权限请求来问你。

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

**上下文与速度**
- 顶栏快速查看：上下文占用环与百分比（接近满时变色），以及输出速度（tok/s，生成时实时更新）；悬停显示具体 token 数，点击或 ⌘I 打开详情
- 每轮页脚：用时 · 输出 token 数 · 速度
- 详情面板（"Context & speed"）：
  - 上下文窗口：已用 / 总量 / 剩余，以及最近一次请求里缓存输入、新输入、输出各占多少
  - 速度：上一轮、首 token 延迟、本对话平均，以及最近 8 轮的明细表
  - token 明细：最近一次请求和整个对话的输入（含缓存命中率）、输出（含推理），Claude 还显示费用
  - Codex 对话里可直接压缩上下文
- 数据来源：Codex 用 `thread/tokenUsage/updated` 的完整拆分；Claude 等 ACP agent 用 `usage_update`（上下文占用、费用）加每轮 `session/prompt` 响应里的 `usage`（本轮 token）。两种报告的到达顺序不固定，所以合并而不是覆盖
- 速度按流式文本计算：从文本估算 token 数，只计文本在流动的时间；超过 2 秒的停顿（运行工具、发起新请求）不算。不用后端报告的输出 token 数来算速度，因为其中包含不流式显示的工具参数和隐藏推理，会把速度算高。输出 token 数在后端报告时用准确值，否则标 ≈ 表示估算

**对话管理**
- 侧边栏按日期分组、搜索、置顶；每条对话可重命名 / 置顶 / 归档；⌘B 折叠为图标栏（宽度动画）
- 顶栏：模型选择、标题、token 与上下文占比、对话菜单（重命名、置顶、代码审查、压缩上下文、归档）
- 代码审查（`review/start`）与压缩上下文（`thread/compact/start`）

**外观与操作**
- 新消息上浮淡入；历史加载时显示骨架屏；Toast 通知
- 命令面板 ⌘K：动作、对话跳转、访问模式、主题
- 设置 ⌘,：界面语言（跟随系统 / English / 中文，即时切换）、主题（跟随系统 / 浅色 / 深色）、默认访问模式、Codex 路径（保存后重启 Codex）
- 设置保存在 `~/Library/Application Support/Atrium/settings.json`
- 界面语言：默认跟随系统（macOS 读首选语言，其他平台读 `LANG` 等变量），命令面板里也能切换。只翻译界面；agent 用你写给它的语言回答，写进 `.atrium/` 的文件保持英文，方便各家 agent 读

**订阅（ChatGPT Plus / Pro）相关**

Atrium 以 `experimentalApi` 连接 Codex app-server，下面这些都走的是 Codex 自己的协议，用的就是你在 Codex 里登录的 ChatGPT 订阅，不需要另配 API key。

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
| ⌘N | 新对话（在项目里：新工作线） |
| ⌘K | 命令面板 |
| ⌘, | 设置 |
| ⌘B | 折叠 / 展开侧边栏 |
| ⌘L | 聚焦输入框 |
| ⌘⇧A | 添加附件 |
| ⌘. | 停止回答 |
| ⌘I | 上下文与速度详情 |
| Tab | 采纳预测的下一句（输入框为空且有预测时） |
| Enter / ⇧Enter | 发送 / 换行 |

mock 后端的触发词：消息里带 `run`（命令 + 审批）、`edit`（文件修改）、`plan`（计划）、`long`（长回答，方便测停止和插话）、`fail`（失败的轮次）、`ask`（Codex 提问）、`image`（生成图片）、`search`（联网搜索）、`agents`（子代理）。mock 以 Pro 账号登录，并像 Codex 0.169 一样对 `thread/prediction/request` 回复 not implemented，从而走预测的 fork 路径。

**macOS 启动时的 PATH**：从 Finder 启动拿不到 shell 的 `PATH`，所以启动时向登录 shell 询问 `PATH`，并搜索 Homebrew / npm / Volta 等常见位置；也可在设置或 `CODEX_BIN` 里指定。

## 结构

```
crates/
  atrium-core/   与后端无关的模型：ThreadMeta / Item / AgentEvent / ConfigOption，以及把事件折叠成扁平行列表的 Transcript
  agent-rpc/        共享传输层：子进程 stdio 上的行分隔 JSON-RPC（可选 JSON-RPC 2.0），查找可执行文件；node：atrium-node 客户端（REST + 断线续传的 WebSocket）；含 mock-node
  codex-client/     codex app-server 客户端与协议翻译；含 mock-codex
  acp-client/       ACP 客户端：握手、会话（new/load/resume/list/prompt/cancel/设置）、权限；把 session/update 翻译成 AgentEvent（为没有 id 的 ACP 合成轮次与条目）；含 mock-acp（可模仿 claude / deepseek / antigravity）
  atrium-project/   项目模型（无 GUI）：中庭文件、任务与路线图、三层角色、公告、范围匹配；agent（agent 可做的操作）、mcp（`atrium` MCP 服务）、skill
  atrium-app/    GPUI 应用：i18n（以英文原文为键的中文表）、Workspace（状态与后端调用；workspace_project 管项目与工作线）+ ui/（sidebar、chat、composer、rows、project、调色板与设置）
```

数据流只有一个方向：

```
codex app-server ──JSON-RPC──▶ codex-client::translate ─┐
ACP agents      ──JSON-RPC 2.0─▶ acp-client::Translator ─┴─AgentEvent──▶ Transcript ──▶ UI
        ▲                                                                              │
        └──────────── turn/start · turn/interrupt · 审批应答 ◀──── Workspace ◀─────────┘
```

`third_party/ely-gpui-components/` 是 Ely 的一份内置副本（通过 `[patch]` 替换上游）：Ely 的聊天、审批、工具卡片组件里原本写死英文，这份副本把它们改为走 Ely 自带的 i18n 目录并补齐了中文。改动说明见其中的 `ATRIUM.md`，适合整理成 PR 提给上游，合并后即可删掉这份副本。

UI 只认识 `atrium-core` 的类型，不认识任何后端协议。任何实现了 ACP 的 agent（如 Gemini CLI）都能以同样方式接入：在 `agents.rs` 里加一条配置即可。Codex 协议标注为实验性、每个版本都会加字段，所以翻译层用 `serde_json::Value` 宽松读取：未知字段忽略，未知条目类型显示为普通系统行，不会让整条消息失败。

## 测试

```bash
cargo test --workspace
```

- `atrium-app`：每一处 `t("…")` / `tf("…")` 都有中文、占位符一致、运行时才出现的文字（访问模式、目标状态等）也都有译文
- `agent-rpc`：节点客户端对 mock-node 的端到端测试（进程输入输出、按序号续传、断线自动重连不丢不重、JSON-RPC 走远程连接、文件只限根目录内），以及 `--ignored` 的真实节点验收：`ATRIUM_TEST_NODE=http://127.0.0.1:7381 cargo test -p agent-rpc --test node -- --ignored`
- `atrium-core`：Transcript 的归并规则（流式拼接、乐观消息去重、审批、失败与断连）；速度测量（首 token、停顿不计时、按轮次的输出数）与用量报告的乱序合并
- `acp-client`：翻译器单元测试（流式文本与工具交替、历史回放切分轮次、diff 与权限、分组选项与模式），以及四条走真实子进程（mock-acp）的端到端测试：Claude 的完整一轮（计划 + 读文件 + 带审批的命令 + diff）、拒绝命令、Claude 回放历史与 DeepSeek 恢复会话、修改设置与中途取消；另有一条 `--ignored` 测试可对真实 agent 做握手：`ATRIUM_ACP="dsh --profile acp" cargo test -p acp-client -- --ignored real_agent`
- `codex-client`：JSON-RPC 请求/响应匹配、协议翻译，以及九条走真实子进程的端到端测试：带审批的完整一轮、中断、附件 + 计划 + 用时、重命名 + 压缩 + 审查 + 文件搜索、预测（fork 回退）、插话、编辑/分支/全文搜索、目标 + 账号 + 用量 + 登录登出 + 记忆、提问 + 图片 + Plan 模式 + Fast

## 下一步

借鉴 Claude 桌面版（[文档](https://code.claude.com/docs/en/desktop.md)）：

- 已做：系统通知（没在看的对话做完、失败、需要你时弹出，设置里可关）；改动面板（`Ctrl/⌘+Shift+D`，按文件看 diff、点行写批注、一次发回给 agent）
- 交给 Codex：每条工作线独立 worktree（[`docs/worktree-implementation.md`](docs/worktree-implementation.md)）；手机通过 tailnet 查看和批准（[`docs/remote-view-implementation.md`](docs/remote-view-implementation.md)，网页部分由界面这边做）
- 之后：分屏和多窗口同时看几条工作线；每条工作线的后台任务面板（正在跑的命令、子 agent）；负责人建议的下一批任务一键开工；PR 与 CI 状态挂在负责人卡片上；前端负责人的应用预览截图；按文件夹记住权限模式；保存的环境（文件夹 + 环境变量）；统一管理所有 agent 的 MCP 和 skill；对话显示档位（普通 / 思考 / 详细）；上下文卡片里的"压缩"按钮
- 原有：中庭写入的 MCP 工具已完成；编排内核独立为 daemon；ACP agent 的插话、agent 内登录；场景（Scene）配置与网格面板
