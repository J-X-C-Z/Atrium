//! The interface's language. Strings are written in English and looked up
//! in a table of Simplified Chinese when Chinese is shown; a string the
//! table lacks shows in English (a test keeps the table complete).
//!
//! `t("Settings")` for a plain string, `tf("{n} new", &[("n", &n)])` for one
//! with values in it. Ely's own component copy follows the same choice
//! through its `I18n` global.

use std::{
    collections::HashMap,
    fmt::Display,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Deserialize, Serialize};

/// The language preference, as saved in settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    /// The system's language, when it is Chinese; English otherwise.
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "en")]
    English,
    #[serde(rename = "zh-CN")]
    Chinese,
}

static CHINESE: AtomicBool = AtomicBool::new(false);

/// Whether Chinese is shown.
pub fn chinese() -> bool {
    CHINESE.load(Ordering::Relaxed)
}

/// Shows `language` from now on; returns the locale tag for Ely.
pub fn apply(language: Language) -> &'static str {
    let chinese = match language {
        Language::Chinese => true,
        Language::English => false,
        Language::System => system_prefers_chinese(),
    };
    CHINESE.store(chinese, Ordering::Relaxed);
    if chinese { "zh-CN" } else { "en-US" }
}

/// Whether the system asks for Chinese: the macOS language list, else the
/// usual locale variables.
pub fn system_prefers_chinese() -> bool {
    #[cfg(target_os = "macos")]
    if let Ok(output) = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        if let Some(first) = text
            .split(['"', '\n', ',', '(', ')'])
            .map(str::trim)
            .find(|s| !s.is_empty())
        {
            return first.starts_with("zh");
        }
    }
    ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"]
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .find(|value| !value.is_empty())
        .is_some_and(|value| value.starts_with("zh"))
}

fn table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| ZH.iter().copied().collect())
}

/// `english`, in the language shown.
pub fn t(english: &'static str) -> &'static str {
    if !chinese() {
        return english;
    }
    table().get(english).copied().unwrap_or(english)
}

/// Text made at run time (an effort level, a status word), in the language
/// shown when the table has it.
pub fn t_any(english: &str) -> String {
    if !chinese() {
        return english.to_string();
    }
    table()
        .get(english)
        .map_or_else(|| english.to_string(), |zh| zh.to_string())
}

/// `template` in the language shown, each `{name}` filled from `args`.
pub fn tf(template: &'static str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = t(template);
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let name = &after[..close];
        match args.iter().find(|(arg, _)| *arg == name) {
            Some((_, value)) => out.push_str(&value.to_string()),
            None => {
                out.push('{');
                out.push_str(name);
                out.push('}');
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// English → Simplified Chinese.
#[rustfmt::skip]
const ZH: &[(&str, &str)] = &[
    ("Settings…", "设置…"),
    ("Quit Atrium", "退出 Atrium"),
    ("File", "文件"),
    ("New Chat", "新对话"),
    ("Attach Files…", "添加附件…"),
    ("Restart Codex", "重启 Codex"),
    ("Chat", "对话"),
    ("Command Palette", "命令面板"),
    ("Focus Composer", "聚焦输入框"),
    ("Stop Answer", "停止回答"),
    ("Context & Speed", "上下文与速度"),
    ("Review Uncommitted Changes", "审查未提交的改动"),
    ("Compact Conversation", "压缩对话"),
    ("View", "视图"),
    ("Toggle Sidebar", "显示/隐藏侧边栏"),
    ("Codex default", "Codex 默认"),
    ("Plan usage: {used}% of the closest limit", "套餐用量：最近的限额已用 {used}%"),
    ("Rename…", "重命名…"),
    ("Unpin", "取消置顶"),
    ("Pin to top", "置顶"),
    ("Review uncommitted changes", "审查未提交的改动"),
    ("Compact conversation", "压缩对话"),
    ("Set a goal…", "设定目标…"),
    ("Branch into a new chat", "分支到新对话"),
    ("Use memory", "使用记忆"),
    ("Context & speed", "上下文与速度"),
    ("Archive", "归档"),
    ("This computer", "这台电脑"),
    ("{device} · offline", "{device} · 离线"),
    ("No agents there", "那里没有 agent"),
    ("Reconnecting to {device}… ({attempt})", "正在重连 {device}…（第 {attempt} 次）"),
    ("The agent on {device} stopped", "{device} 上的 agent 已停止"),
    ("Working · {secs}s", "工作中 · {secs} 秒"),
    ("New chat (⌘N)", "新对话 (⌘N)"),
    ("Could not start Codex", "无法启动 Codex"),
    ("{used} of {budget} tokens", "{used} / {budget} tokens"),
    ("{used} tokens", "{used} tokens"),
    ("Pause", "暂停"),
    ("Resume", "继续"),
    ("Change the goal", "修改目标"),
    ("Clear the goal", "清除目标"),
    ("{name} is ready in {folder}", "{name} 已就绪，位于 {folder}"),
    ("Starting {name}…", "正在启动 {name}…"),
    ("{name}: the device is offline.", "{name}：设备离线。"),
    ("{name} is not installed. {install}", "{name} 未安装。{install}"),
    ("{name} is not running. Pick it again to restart it.", "{name} 未运行。重新选择它即可重启。"),
    ("Add to what Codex is doing: it reads this at its next step", "补充给正在工作的 Codex：它会在下一步读到"),
    ("{agent} is working. Wait for it, or stop it to change course", "{agent} 正在工作。等它完成，或先停止再改方向"),
    ("Edit your message", "编辑你的消息"),
    ("Describe the change: Codex plans first and waits for you", "描述要做的改动：Codex 会先给出计划，等你确认"),
    ("Ask {agent} anything. @ to add files, / for commands", "问 {agent} 任何问题。@ 添加文件，/ 使用命令"),
    ("Attach", "添加"),
    ("Plan", "计划"),
    ("Plan mode: Codex proposes a plan and waits before changing anything", "Plan 模式：Codex 先提出计划，确认后才改动"),
    ("done", "完成"),
    ("failed", "失败"),
    ("queued", "排队中"),
    ("Role", "角色"),
    ("Lead", "负责人"),
    ("Controller", "主控"),
    ("New controller", "新建主控"),
    ("Disable local preview commands", "停用本地预览命令"),
    ("Review local preview commands…", "审阅本地预览命令…"),
    ("Could not read preview commands: {error}", "无法读取预览命令：{error}"),
    ("Allow these commands to run locally after lead phases?", "允许在负责人完成阶段后在本机运行这些命令吗？"),
    ("Allow for this project", "允许此项目运行"),
    ("New lead", "新建负责人"),
    ("The project's goal", "项目目标"),
    ("The controller's plan: the milestones in order and which lead owns each.", "主控的计划：按顺序排列的里程碑，以及每个里程碑由哪位负责人负责。"),
    ("The controller splits the project into leads, gives each a roadmap and keeps the plan. It writes no code, and wakes only when a lead reports.", "主控把项目拆给几位负责人，给每位一份路线图，并维护整体计划。它不写代码，只在负责人汇报时被唤醒。"),
    ("A long-running session that owns one part of the project. It gets its goal, its scope and the atrium's rules, then works on its own, handing pieces to sub-agents when it helps.", "长期负责项目某一部分的会话。它拿到目标、范围和中庭规则后自主推进，必要时把独立的小块交给子 agent。"),
    ("Start with a controller", "从主控开始"),
    ("Give the controller the project's goal. It splits the work into leads, each owning a part with its own roadmap, and leads hand self-contained pieces to sub-agents. Or add leads yourself.", "把项目目标交给主控：它把工作拆给几位负责人，每位负责一部分并有自己的路线图；负责人再把独立的小块交给子 agent。你也可以自己添加负责人。"),
    ("Roadmap · next: {step}", "路线图 · 下一步：{step}"),
    ("Roadmap done", "路线图已完成"),
    ("{n} leads · {finished} done", "{n} 位负责人 · {finished} 位已完成"),
    ("{done}/{total} milestones", "里程碑 {done}/{total}"),
    ("{n} leads about to start", "{n} 位负责人即将启动"),
    ("Review progress", "检查进度"),
    ("The controller proposes {n} leads", "主控提议了 {n} 位负责人"),
    ("Start all", "全部开始"),
    ("same agent", "同主控"),
    ("Turn down", "驳回"),
    ("Sub-agents · {running} running · {queued} queued", "子 agent · {running} 个运行中 · {queued} 个排队"),
    ("Approve the controller's leads before they start", "主控提议的负责人需经我批准才启动"),
    ("Give each agent its own worktree", "为每个代理使用独立的 Git 工作树"),
    ("Worktree operation failed: {error}", "工作树操作失败：{error}"),
    ("Run unattended, around the clock", "无人值守，24 小时持续运行"),
    ("Unattended", "无人值守"),
    ("Notifications", "通知"),
    ("A system notification when a conversation you are not looking at finishes, fails or needs you.", "你没在看的对话做完、失败或需要你时，弹出系统通知。"),
    ("Notify me", "通知我"),
    ("More", "更多"),
    ("A prompt Atrium sends by itself at a set time, such as a morning check of CI. They run while Atrium is open; one missed while it was closed runs when it opens, the same day.", "到点由 Atrium 自动发出的一条消息，比如每天早上看一眼 CI。Atrium 开着时按时运行；关着时错过的，当天打开 Atrium 后补跑。"),
    ("Atrium cannot open {url}", "Atrium 打不开 {url}"),
    ("Atrium looks for a newer release on GitHub once a day and shows it at the bottom of the sidebar.", "Atrium 每天去 GitHub 查一次新版本，有新版本时显示在侧边栏底部。"),
    ("Changed files", "改动的文件"),
    ("Check for updates", "自动检查更新"),
    ("Check now", "立即检查"),
    ("Command, such as npm run dev", "启动命令，比如 npm run dev"),
    ("Copied as Markdown", "已复制为 Markdown"),
    ("Copy as Markdown", "复制为 Markdown"),
    ("Could not check for updates.", "检查更新失败。"),
    ("Download", "下载"),
    ("Editor", "编辑器"),
    ("Exit code {code}", "退出码 {code}"),
    ("Export as Markdown…", "导出为 Markdown…"),
    ("Folder: {folder}", "文件夹：{folder}"),
    ("Name, such as Morning CI check", "名称（可选）"),
    ("No editor found. Install the command-line launcher of VS Code, Cursor, Zed, Windsurf or Sublime Text.", "没有找到编辑器。请安装 VS Code、Cursor、Zed、Windsurf 或 Sublime Text 的命令行启动器。"),
    ("Open in {editor}", "在 {editor} 中打开"),
    ("Open preview", "打开预览"),
    ("Question", "问题"),
    ("Restart", "重启"),
    ("Run", "运行"),
    ("Run now", "立即运行"),
    ("Run works in a conversation's folder on this computer.", "「运行」在本机对话所在的文件夹里启动项目。"),
    ("Saved to {path}", "已保存到 {path}"),
    ("Scheduled chats", "定时对话"),
    ("Show in file manager", "在文件管理器中显示"),
    ("Starts the project here, such as its dev server, and opens the address it prints. The command is remembered for this folder.", "在这里启动项目（比如开发服务器），并打开它输出的地址。命令会按文件夹记住。"),
    ("Stop", "停止"),
    ("Stopped with exit code {code}", "已停止，退出码 {code}"),
    ("That project is no longer open.", "这个项目已经不在打开的项目里了。"),
    ("Tool", "工具"),
    ("Updates", "更新"),
    ("Version {version}", "版本 {version}"),
    ("What to ask, such as: Check last night's CI runs and summarise the failures", "要发的内容，比如：看一下昨晚的 CI，总结失败的原因"),
    ("Where files and folders open from the changes view and the chat menu.", "从改动面板和对话菜单打开文件、文件夹时用的编辑器。"),
    ("Write the command that starts the project.", "请填写启动项目的命令。"),
    ("Write the time as HH:MM, such as 09:00.", "时间请写成「时:分」，比如 09:00。"),
    ("Write what to ask.", "请填写要发的内容。"),
    ("You have the latest Atrium.", "已经是最新版本。"),
    ("declined", "已拒绝"),
    ("edited", "已修改"),
    ("Atrium {version} is out", "Atrium {version} 已发布"),
    ("Serving at {url}", "正在运行：{url}"),
    ("Runs in {folder} with {agent}", "在 {folder} 用 {agent} 运行"),
    ("Every day", "每天"),
    ("Weekdays", "工作日"),
    ("Weekends", "周末"),
    ("Scheduled chat “{name}” started.", "定时对话「{name}」已开始。"),
    ("Scheduled chat “{name}”: its folder {folder} is gone.", "定时对话「{name}」：文件夹 {folder} 不存在了。"),
    ("Exported from Atrium on {date}", "由 Atrium 导出于 {date}"),
    ("… {n} more lines", "…… 还有 {n} 行"),
    ("A terminal opens in a conversation's folder on this computer.", "终端会在本机对话所在的文件夹里打开。"),
    ("Commit all", "全部提交"),
    ("Commit message", "提交说明"),
    ("Committed {id}", "已提交 {id}"),
    ("Committing…", "正在提交…"),
    ("Could not make a worktree: {error}", "无法创建工作树：{error}"),
    ("Create PR", "创建 PR"),
    ("Merge this worktree into the main branch", "把这个工作树合并回主分支"),
    ("Merged into {branch}.", "已合并到 {branch}。"),
    ("Nothing uncommitted.", "没有未提交的改动。"),
    ("Open in editor", "在编辑器中打开"),
    ("Opening a pull request…", "正在创建 PR…"),
    ("Pull request opened: {url}", "PR 已创建：{url}"),
    ("Push", "推送"),
    ("Pushed", "已推送"),
    ("Pushing…", "正在推送…"),
    ("Remove this worktree", "删除这个工作树"),
    ("Restore undone.", "已撤销恢复。"),
    ("Start in its own worktree", "在独立工作树中开始"),
    ("Stop the agent before removing its worktree.", "请先停止 agent，再删除它的工作树。"),
    ("Stop the agent before undoing files.", "请先停止 agent，再撤销文件改动。"),
    ("Terminal", "终端"),
    ("Undid the changes to {path}", "已撤销 {path} 的改动"),
    ("Undo", "撤销"),
    ("Undo the last file restore", "撤销上次的文件恢复"),
    ("Undo this file's changes", "撤销这个文件的改动"),
    ("Undo this turn's file changes", "撤销这一轮的文件改动"),
    ("Don't send", "不发送"),
    ("Editing this kind of file is not available yet.", "这类文件暂时还不能在这里编辑。"),
    ("Keep mine", "保留我的"),
    ("Load theirs", "载入磁盘上的版本"),
    ("Open a document, spreadsheet, slides, PDF, picture or Markdown file the agent made. Word, Excel and PowerPoint files can be edited here, and your changes go to the agent with your next message.", "打开 agent 生成的文档、表格、幻灯片、PDF、图片或 Markdown 文件。Word、Excel 和 PPT 可以直接在这里修改，你的改动会随下一条消息发给 agent。"),
    ("Open a file…", "打开文件…"),
    ("Open with its app", "用默认程序打开"),
    ("Opening…", "正在打开…"),
    ("Preview", "预览"),
    ("Pages", "页面"),
    ("Previews of this kind of file are not available yet.", "这类文件暂时还不能预览。"),
    ("Reload", "重新加载"),
    ("Rows, columns and new tables can't be changed here yet; edit the cells' text.", "这里暂时不能增删行列或新建表格，只能修改单元格里的文字。"),
    ("Save a copy…", "另存一份…"),
    ("Save or discard your edits to this document first.", "请先保存或放弃对这份文档的修改。"),
    ("Saved.", "已保存。"),
    ("Saving Word, Excel and PowerPoint files is not available yet. Your edits stay here until you close the file.", "保存 Word、Excel 和 PPT 的功能还没完成。你的修改会保留在这里，直到关闭这个文件。"),
    ("Saving when the agent stops", "agent 停下后保存"),
    ("Showing the first {n} rows", "只显示前 {n} 行"),
    ("Showing {n} of {total} pages", "显示 {total} 页中的 {n} 页"),
    ("Slide {n}", "第 {n} 张幻灯片"),
    ("Slides take text edits here; add or remove boxes in the original app.", "幻灯片在这里只能改文字；增删文本框请在原程序里操作。"),
    ("Some parts may look different from the original app.", "部分内容可能和原程序里显示的不一样。"),
    ("The agent is working. Your edits will be saved when it stops.", "agent 正在运行，等它停下后会自动保存你的修改。"),
    ("The file changed on disk since you opened it, probably by the agent.", "打开之后这个文件在磁盘上被改过了，很可能是 agent 改的。"),
    ("The file changed on disk while you were editing it.", "你编辑期间，这个文件在磁盘上被改过了。"),
    ("The file is back as it was before you saved.", "文件已恢复到你保存之前的样子。"),
    ("The file is password protected.", "文件有密码保护。"),
    ("The file is too big to open here.", "文件太大，无法在这里打开。"),
    ("The file is too big to preview.", "文件太大，无法预览。"),
    ("The file is too big to show here.", "文件太大，无法在这里显示。"),
    ("The file looks damaged.", "文件好像损坏了。"),
    ("The preview took too long.", "预览耗时太久，已放弃。"),
    ("There is no preview for this kind of file.", "这类文件没有预览。"),
    ("This is an old Office format; open it in its app, or save it as a newer one.", "这是旧版 Office 格式，请用原程序打开，或另存为新格式。"),
    ("This paragraph holds tracked changes, comments or fields. Edit it in the original app.", "这一段含有修订、批注或域，请在原程序里编辑。"),
    ("This part is shown as it is and can't be edited here.", "这部分只能原样显示，不能在这里编辑。"),
    ("Undo save", "撤销保存"),
    ("Formulas keep their last results here; Excel recalculates them when it opens the file.", "公式在这里显示的是上次计算的结果，Excel 打开文件时会重新计算。"),
    ("Binary file", "二进制文件"),
    ("Your changes to {files} go to the agent with your next message.", "你对 {files} 的改动会随下一条消息发给 agent。"),
    ("field", "域"),
    ("footnote", "脚注"),
    ("hidden", "已隐藏"),
    ("image", "图片"),
    ("Undoing…", "正在撤销…"),
    ("Worktree removed. Its branch is kept.", "工作树已删除，分支保留。"),
    ("Write a commit message first.", "请先写提交说明。"),
    ("detached", "游离 HEAD"),
    ("not pushed yet", "尚未推送"),
    ("up to date with {up}", "与 {up} 同步"),
    ("{folder} · own worktree", "{folder} · 独立工作树"),
    ("{n} behind", "落后 {n} 个提交"),
    ("{n} to push", "{n} 个提交待推送"),
    ("Files are back as they were before message {n}. Undo it from the chat menu.", "文件已恢复到第 {n} 条消息之前的状态。可在对话菜单里撤销这次恢复。"),
    ("Phone", "手机"),
    ("Follow projects, answer approvals and post notices from your phone, over Tailscale only. Pushes go through ntfy and carry only project and workstream names.", "在手机上跟进项目、处理审批、发布公告，只走 Tailscale。推送经过 ntfy，只带项目和工作线的名字。"),
    ("Let my other devices view projects", "允许我的其他设备查看项目"),
    ("On the phone: <this computer>:7879", "手机上打开 <本机 Tailscale 名>:7879"),
    ("Open here", "在这台电脑打开"),
    ("ntfy topic, such as atrium-7f3k, or a full URL", "ntfy 主题，比如 atrium-7f3k，或完整网址"),
    ("Pushes go to {topic}", "推送发到 {topic}"),
    ("Merge", "合并"),
    ("Changes", "改动"),
    ("{n} files", "{n} 个文件"),
    ("No file changes in this conversation yet.", "这段对话还没有改过文件。"),
    ("new", "新文件"),
    ("deleted", "已删除"),
    ("Add comment", "添加批注"),
    ("Comment on this line…", "对这一行写批注…"),
    ("{n} more lines not shown", "还有 {n} 行未显示"),
    ("{n} comments", "{n} 条批注"),
    ("Discard", "丢弃"),
    ("Send comments", "发送批注"),
    ("Activity", "活动"),
    ("Side by side", "并排"),
    ("Activity and side by side", "活动与并排查看"),
    ("running", "运行中"),
    ("Open a conversation to see its activity.", "打开一段对话，查看它的后台活动。"),
    ("Running now · {n}", "正在运行 · {n}"),
    ("Nothing in the background.", "后台没有在跑的东西。"),
    ("Stop this turn", "停止这一轮"),
    ("Sub-agents · {n}", "子 agent · {n}"),
    ("Recently", "最近"),
    ("Watch this conversation", "并排查看这段对话"),
    ("Watch up to four conversations here while you work in another: each shows its state, what it is running and what it said last.", "在这里同时盯最多四段对话，你可以在别的对话里干活：每段都显示状态、正在跑什么、最后说了什么。"),
    ("Open", "打开"),
    ("Stop watching", "不再并排"),
    ("Suggested next", "建议的下一步"),
    ("Start all {n}", "全部开始（{n}）"),
    ("Dismiss", "关闭"),
    ("Send here", "发到这里"),
    ("Start in a new conversation", "在新对话里开始"),
    ("Name, such as docs", "名称，如 docs"),
    ("Command, such as npx -y @acme/docs-mcp", "命令，如 npx -y @acme/docs-mcp"),
    ("Name, such as staging", "名称，如 staging"),
    ("One KEY=value per line", "每行一个 KEY=value"),
    ("Off", "已停用"),
    ("On", "已启用"),
    ("Remove", "移除"),
    ("None: agents get only their own environment", "不使用：agent 只用自己的环境"),
    ("Restart ACP agents", "重启 ACP agent"),
    ("Restart ACP agents to use the new environment.", "重启 ACP agent 以使用新环境。"),
    ("Finish or stop active ACP turns before restarting agents.", "请先等待或停止正在运行的 ACP 对话，再重启 agent。"),
    ("Save environment", "保存环境"),
    ("Install the Atrium skill for every agent", "为所有 agent 安装 Atrium skill"),
    ("Atrium skill installed", "已安装 Atrium skill"),
    ("No skills installed yet.", "还没有安装任何 skill。"),
    ("Share", "共享"),
    ("CI passing", "CI 通过"),
    ("CI failing", "CI 失败"),
    ("CI running", "CI 运行中"),
    ("merged", "已合并"),
    ("Latest preview. Click to open it full size.", "最新预览，点击查看大图。"),
    ("Shared with {n} more agent(s)", "已共享给另外 {n} 个 agent"),
    ("MCP servers", "MCP 服务器"),
    ("Given to every agent, Codex and ACP alike, in every new or reopened session on this computer. Agents on other devices are left out.", "这台电脑上每个新建或重新打开的会话都会带上，Codex 和 ACP agent 一视同仁；其他设备上的 agent 不带。"),
    ("Environments", "环境"),
    ("Variables new sessions run with, such as an API's address. Codex gets them for its commands; ACP agents read their own environment, so restart them after a change.", "新会话使用的环境变量，比如某个 API 的地址。Codex 会把它们传给自己执行的命令；ACP agent 读取的是自己进程的环境，改动后要重启它们。"),
    ("Skills", "Skill"),
    ("Skills in ~/.claude/skills and ~/.codex/skills. Share one to give every agent the same.", "~/.claude/skills 和 ~/.codex/skills 里的 skill。点“共享”让每个 agent 都有同一份。"),
    ("This agent cannot compact a conversation.", "这个 agent 不支持压缩对话。"),
    ("Normal view", "普通视图"),
    ("Thinking view", "显示思考"),
    ("Verbose view", "详细视图"),
    ("Cycle Normal / Thinking / Verbose", "切换 普通 / 思考 / 详细"),
    ("Finished its turn.", "这一轮做完了。"),
    ("Needs your approval.", "需要你批准。"),
    ("Has a question for you.", "有问题要问你。"),
    ("Its turn failed.", "这一轮失败了。"),
    ("idle", "空闲"),
    ("Pool: {ready} ready · {running} in progress · {done}/{total} done", "任务池：{ready} 待领 · {running} 进行中 · 已完成 {done}/{total}"),
    ("· {n} failed", "· {n} 个失败"),
    ("Sub-agents · {running} running", "子 agent · {running} 个运行中"),
    ("+{n} more in the pool", "任务池里还有 {n} 个"),
    ("Progress", "进度"),
    ("Cards", "卡片"),
    ("No roadmap yet", "还没有路线图"),
    ("Phase {n} of {total}: {title}", "第 {n}/{total} 阶段：{title}"),
    ("All phases done", "全部阶段已完成"),
    ("Next: {step}", "下一步：{step}"),
    ("{n} queued", "{n} 个排队"),
    ("Its roadmap appears here once the controller writes it, or add a Roadmap section to its task file.", "主控写好路线图后会显示在这里；也可以在它的任务文件里加一个 Roadmap 小节。"),
    ("Fast", "快速"),
    ("{agent} · settings in {path}", "{agent} · 设置保存在 {path}"),
    ("{agent} can make mistakes. Review its changes before you ship them.", "{agent} 可能会出错。发布前请检查它的改动。"),
    ("About twice the speed; uses more of your plan's usage", "速度约快一倍，消耗更多套餐额度"),
    ("Effort", "推理强度"),
    ("Options", "选项"),
    ("Attach images or files", "添加图片或文件"),
    ("Roots", "根目录"),
    ("Up one level", "上一级"),
    ("Loading…", "加载中…"),
    ("In {folder}", "{folder} 中"),
    ("Choose a folder…", "选择文件夹…"),
    ("Use this folder", "使用这个文件夹"),
    ("Starting Codex…", "正在启动 Codex…"),
    ("Codex is not running. Restart it from the sidebar.", "Codex 未运行。请从侧边栏重启。"),
    ("Not signed in: sign in with ChatGPT from the sidebar to use your plan.", "未登录：从侧边栏用 ChatGPT 登录后即可使用你的套餐。"),
    ("Stop (⌘.)", "停止 (⌘.)"),
    ("Editing a message: sending replaces it and everything after", "正在编辑消息：发送后会替换它以及之后的内容"),
    ("Cancel", "取消"),
    ("{rate} tok/s", "{rate} tok/s"),
    ("{n} tokens", "{n} tokens"),
    ("Context: {used} of {window} tokens", "上下文：{used} / {window} tokens"),
    ("{n} tokens used", "已用 {n} tokens"),
    ("Last answer: {speed}", "上一轮：{speed}"),
    ("Writing at {speed}", "正在输出：{speed}"),
    ("Cached input", "缓存输入"),
    ("New input", "新输入"),
    ("Output", "输出"),
    (" ({n} cached, {share}%)", "（缓存 {n}，{share}%）"),
    ("Input", "输入"),
    (" ({n} reasoning)", "（推理 {n}）"),
    ("Total", "合计"),
    ("(no message)", "（无消息）"),
    ("Nothing measured yet: send a message first.", "还没有数据：先发一条消息。"),
    ("Context window", "上下文窗口"),
    ("of {n} tokens", "/ {n} tokens"),
    ("{share} used · {left} left", "已用 {share} · 剩余 {left}"),
    ("The context is nearly full. Compact the conversation to make room; older details get summarized.", "上下文快满了。压缩对话可以腾出空间，较早的细节会被概括。"),
    ("The context is nearly full. Ask the agent to /compact, or start a new chat.", "上下文快满了。让 agent 执行 /compact，或开一个新对话。"),
    ("In use", "已占用"),
    ("{n} tokens (the window size isn't reported)", "{n} tokens（未报告窗口大小）"),
    ("{agent} hasn't reported its context yet.", "{agent} 还没有报告上下文。"),
    ("Speed", "速度"),
    ("Last answer", "上一轮"),
    ("Writing now", "正在输出"),
    ("First token", "首个 token"),
    ("Average", "平均"),
    ("Speed is measured on the text as it streams in; pauses for tools and new requests don't count. Counts marked ≈ are estimated from the text.", "速度按流式输出的文字计算，运行工具和发起新请求的停顿不计入。标 ≈ 的数量是根据文字估算的。"),
    ("Last request", "最近一次请求"),
    ("Last turn", "最近一轮"),
    ("This chat", "本对话"),
    ("Cost", "费用"),
    ("Recent turns", "最近几轮"),
    ("Done", "完成"),
    ("Turn", "轮次"),
    ("First", "首 token"),
    ("Took", "用时"),
    ("Looking for devices…", "正在查找设备…"),
    ("Tailnet: {online} online · {nodes} running atrium-node", "Tailnet：{online} 台在线 · {nodes} 台运行 atrium-node"),
    ("Tailnet not read: {why}", "无法读取 Tailnet：{why}"),
    ("Looking…", "查找中…"),
    ("Refresh", "刷新"),
    ("{agent} (starting)", "{agent}（启动中）"),
    ("{agent} (not running)", "{agent}（未运行）"),
    ("Offline: {why}", "离线：{why}"),
    ("No agents", "没有 agent"),
    ("Not installed there: {list}", "那里未安装：{list}"),
    ("No devices yet. Run atrium-node on another computer of your tailnet; it shows up here.", "还没有设备。在 tailnet 里的另一台电脑上运行 atrium-node，它就会出现在这里。"),
    ("Added: {node}", "手动添加：{node}"),
    ("Forget this address", "移除这个地址"),
    ("Add", "添加"),
    ("Search chats", "搜索对话"),
    ("Tell the workstreams something…", "告诉各工作线一些事…"),
    ("Chat name", "对话名称"),
    ("What should Codex keep working toward?", "Codex 要持续朝什么目标推进？"),
    ("Token budget (optional), such as 500000", "Token 预算（可选），例如 500000"),
    ("Set a goal", "设定目标"),
    ("Codex keeps working toward the goal across turns until it is done, blocked, or out of budget.", "Codex 会跨多轮持续推进目标，直到完成、受阻或预算用尽。"),
    ("Long goals use your plan's usage. Pause or clear the goal from the banner at any time.", "长期目标会消耗套餐额度。随时可以在横幅上暂停或清除。"),
    ("Start working", "开始工作"),
    ("ChatGPT account", "ChatGPT 账号"),
    ("Signed in with an API key: usage is billed per token, with no plan limits.", "使用 API key 登录：按 token 计费，没有套餐限额。"),
    ("Signed in through {kind}.", "通过 {kind} 登录。"),
    ("Sign in with ChatGPT to use your Plus or Pro subscription in Codex.", "用 ChatGPT 登录，就能在 Codex 里使用你的 Plus 或 Pro 订阅。"),
    ("{percent}% used{reset}", "已用 {percent}%{reset}"),
    ("Unlimited credits", "不限 credits"),
    ("{balance} credits left", "剩余 {balance} credits"),
    ("Credits available", "有可用 credits"),
    ("No credits", "没有 credits"),
    ("A usage limit is reached; Codex waits until it resets.", "已达用量上限；Codex 会等到重置后继续。"),
    ("Memory: {remembered} chats remembered. Turn it off per chat from the chat menu.", "记忆：已记住 {remembered} 个对话。可以在对话菜单里按对话关闭。"),
    ("Memory is getting ready.", "记忆正在准备中。"),
    ("Account & usage", "账号与用量"),
    ("Sign out", "退出登录"),
    ("Cancel sign-in", "取消登录"),
    ("Sign in with ChatGPT", "用 ChatGPT 登录"),
    ("Starting…", "启动中…"),
    ("Ready", "就绪"),
    ("Found on PATH (leave empty)", "从 PATH 自动查找（留空）"),
    ("Add a node by address, such as studio:7381", "按地址添加节点，例如 studio:7381"),
    ("New chat", "新对话"),
    ("Attach files…", "添加附件…"),
    ("Choose working folder…", "选择工作文件夹…"),
    ("Toggle sidebar", "显示/隐藏侧边栏"),
    ("Settings", "设置"),
    ("Set a goal for this chat…", "为这个对话设定目标…"),
    ("Branch this chat", "分支这个对话"),
    ("Toggle memory for this chat", "开关这个对话的记忆"),
    ("Open a folder as a project…", "把文件夹作为项目打开…"),
    ("New workstream…", "新工作线…"),
    ("Go to chats", "前往对话"),
    ("Toggle Fast mode", "开关 Fast 模式"),
    ("Toggle Plan mode", "开关 Plan 模式"),
    ("Toggle next-message predictions", "开关下一句预测"),
    ("Access: {mode}", "权限：{mode}"),
    ("Theme: match system", "主题：跟随系统"),
    ("Theme: light", "主题：浅色"),
    ("Theme: dark", "主题：深色"),
    ("New chat with {name}", "用 {name} 开始新对话"),
    ("Actions", "操作"),
    ("Agents", "Agents"),
    ("Chats", "对话"),
    ("Access", "权限"),
    ("Appearance", "外观"),
    ("System", "系统"),
    ("Light", "浅色"),
    ("Dark", "深色"),
    ("Match system", "跟随系统"),
    ("Language", "语言"),
    ("The interface's language. Agents answer in whatever language you write to them.", "界面语言。agent 会用你写给它的语言回答。"),
    ("Follow macOS, or keep one look.", "跟随系统，或固定一种外观。"),
    ("Default access for new chats", "新对话的默认权限"),
    ("Suggestions", "建议"),
    ("After each answer Codex guesses your next message; Tab takes it. Each guess uses a little of your plan.", "每次回答后 Codex 会猜你下一句要说什么，按 Tab 采纳。每次猜测会用掉一点套餐额度。"),
    ("Predict my next message", "预测我的下一句"),
    ("Claude Code, DeepSeek Harness and Antigravity run over the Agent Client Protocol. Set a command per agent with `agent_commands` in settings.json.", "Claude Code、DeepSeek Harness 和 Antigravity 通过 Agent Client Protocol 运行。可以在 settings.json 的 `agent_commands` 里为每个 agent 指定启动命令。"),
    ("Devices", "设备"),
    ("Agents on your other computers, through atrium-node over Tailscale. Their chats show the device's name.", "你其他电脑上的 agent，通过 Tailscale 上的 atrium-node 连接。它们的对话会标出设备名。"),
    ("Path to the codex binary. Saving restarts Codex.", "codex 程序的路径。保存后会重启 Codex。"),
    ("Close", "关闭"),
    ("Save", "保存"),
    ("Rename chat", "重命名对话"),
    ("Rename", "重命名"),
    ("A chat needs a name.", "对话需要一个名字。"),
    ("5-hour limit", "5 小时限额"),
    ("Weekly limit", "每周限额"),
    ("{n}-day limit", "{n} 天限额"),
    ("{n}-hour limit", "{n} 小时限额"),
    ("{n}-minute limit", "{n} 分钟限额"),
    ("Usage limit", "用量限额"),
    (" · resets now", " · 即将重置"),
    (" · resets in {mins}m", " · {mins} 分钟后重置"),
    (" · resets in {h}h {mins}m", " · {h} 小时 {mins} 分钟后重置"),
    (" · resets in {d}d {h}h", " · {d} 天 {h} 小时后重置"),
    ("Agent not running", "agent 未运行"),
    ("Working", "工作中"),
    ("Needs you", "需要你"),
    ("Needs you ({n})", "需要你（{n}）"),
    ("Hit an error", "出错了"),
    ("Idle", "空闲"),
    ("Open as a project", "作为项目打开"),
    ("No projects yet.", "还没有项目。"),
    ("A project is a folder that several long-running sessions work on, each toward its own goal.", "项目是一个代码文件夹，由几个长期会话各自朝自己的目标推进。"),
    ("{n} new", "{n} 条新"),
    ("{n} workstreams · {working} working", "{n} 条工作线 · {working} 条工作中"),
    (" · {n} need you", " · {n} 条需要你"),
    ("New workstream", "新工作线"),
    ("Open the atrium folder", "打开中庭文件夹"),
    ("Remove from Atrium", "从 Atrium 移除"),
    ("is waiting for an approval or an answer", "在等你审批或回答"),
    ("is waiting on {n} approvals or answers", "有 {n} 项等你审批或回答"),
    ("Start a workstream", "开始一条工作线"),
    ("Each workstream is a long-running session with its own goal and scope, such as the frontend, a backend feature, or performance work. They work on their own and align through the atrium on the right.", "每条工作线是一个长期会话，有自己的目标和负责范围，比如前端、某个后端功能或性能优化。它们各自推进，通过右侧的中庭对齐信息。"),
    ("Plan done", "计划已完成"),
    ("Focus:", "当前："),
    ("1 new notice", "1 条新公告"),
    ("{n} new notices", "{n} 条新公告"),
    ("1 change outside its scope", "1 处越界改动"),
    ("{n} changes outside its scope", "{n} 处越界改动"),
    ("{share}% context", "上下文 {share}%"),
    ("Goal done", "目标已完成"),
    ("No goal set", "未设定目标"),
    ("Scope: {scope}", "范围：{scope}"),
    ("Hand over notices", "移交公告"),
    ("Carry on", "继续推进"),
    ("You", "你"),
    ("everyone", "所有人"),
    ("Notices", "公告"),
    ("Brief", "共识"),
    ("Contracts", "接口"),
    ("Status", "状态"),
    ("Everyone", "所有人"),
    ("To {who}", "发给 {who}"),
    ("Post", "发布"),
    ("No notices yet. Workstreams post here when they change something others depend on.", "还没有公告。工作线改动了别人依赖的东西时会发在这里。"),
    ("Shared conventions and decisions. Every workstream reads it first.", "共同的约定和决定。每条工作线开工前都会先读。"),
    ("Interfaces between workstreams. Whoever changes one updates its file and posts a notice.", "工作线之间的接口。谁改了接口，谁就更新对应文件并发公告。"),
    ("Open folder", "打开文件夹"),
    ("No contracts yet.", "还没有接口文件。"),
    ("No status yet ({path}).", "还没有状态（{path}）。"),
    ("Workstreams write their status here.", "各工作线会在这里写下自己的状态。"),
    ("Atrium", "中庭"),
    ("Edit", "编辑"),
    ("Goal: {goal}", "目标：{goal}"),
    ("Take out of the project (the chat stays, under Chats)", "移出项目（对话保留在“对话”中）"),
    ("Review notice delivery", "处理公告投递"),
    ("Notice delivery is uncertain", "公告投递状态不确定"),
    ("Check the conversation first. Retry can send duplicate notices. It will never resend your earlier message.", "请先核对对话。重试可能重复发送公告，但不会重发你之前的消息。"),
    ("Mark notices received", "确认已收到公告"),
    ("Retry notices only", "仅重试公告"),
    ("Notice delivery changed. Check the current status and try again.", "公告投递状态已变化，请核对后重试。"),
    ("Could not resolve notice delivery: {error}", "无法处理公告投递：{error}"),
    ("Hand over 1 new notice", "移交 1 条新公告"),
    ("Hand over {n} new notices", "移交 {n} 条新公告"),
    ("Name, such as Frontend", "名称，例如“前端”"),
    ("Its long-term goal: what done looks like", "它的长期目标：做到什么程度算完成"),
    ("Scope: paths it owns, such as web/**, src/ui", "范围：它负责的路径，例如 web/**, src/ui"),
    ("Codex", "Codex"),
    ("A long-running session in this project. It gets its goal, its scope and the atrium's rules, then works on its own.", "项目里的一个长期会话。它会拿到自己的目标、范围和中庭规则，然后自己推进。"),
    ("Name", "名称"),
    ("Agent", "Agent"),
    ("Goal", "目标"),
    ("Scope", "范围"),
    ("Changes it makes in another workstream's scope are flagged and posted to that workstream.", "它改到其他工作线范围内的文件时，会被标出并发公告给那条工作线。"),
    ("Start", "开始"),
    ("Give the workstream a name and a goal.", "请给工作线填写名称和目标。"),
    ("the whole repository", "整个仓库"),
    ("Thinking · {secs}s", "思考中 · {secs} 秒"),
    ("Thinking", "思考中"),
    ("Stopped", "已停止"),
    ("Failed", "失败"),
    ("Branch into a new chat from here", "从这里分支到新对话"),
    ("{m}m {s}s", "{m} 分 {s} 秒"),
    ("Edit and resend", "编辑并重发"),
    ("Editing: sending replaces this message and what follows", "编辑中：发送后会替换这条消息及之后的内容"),
    ("{agent} wants to run a command", "{agent} 想运行一条命令"),
    ("$ {command}\n  in {cwd}", "$ {command}\n  位于 {cwd}"),
    ("{agent} wants to change files", "{agent} 想修改文件"),
    ("Write access under {root}", "{root} 下的写入权限"),
    ("Apply the proposed edits", "应用建议的修改"),
    ("Allow this tool to run", "允许运行这个工具"),
    ("Allowed once", "已允许一次"),
    ("Allowed for this session", "本会话内始终允许"),
    ("Declined", "已拒绝"),
    ("Declined and stopped", "已拒绝并停止"),
    ("No longer needed", "已不需要"),
    ("Something went wrong", "出了点问题"),
    ("Running", "运行中"),
    ("Ran", "已运行"),
    ("Searching the web", "正在搜索网页"),
    ("Searched the web", "已搜索网页"),
    ("Plan · {text}", "计划 · {text}"),
    ("Creating image…", "正在生成图片…"),
    ("The image could not be made", "图片生成失败"),
    ("No answer needed", "不需要回答"),
    ("Answered: {said}", "已回答：{said}"),
    ("Or type an answer", "或者输入回答"),
    ("Codex has a question", "Codex 有个问题"),
    ("Send answer", "发送回答"),
    ("{backend} · not signed in", "{backend} · 未登录"),
    ("Codex is not running", "Codex 未运行"),
    ("Show sidebar (⌘B)", "显示侧边栏 (⌘B)"),
    ("Search (⌘K)", "搜索 (⌘K)"),
    ("Settings (⌘,)", "设置 (⌘,)"),
    ("Hide sidebar (⌘B)", "隐藏侧边栏 (⌘B)"),
    ("Projects", "项目"),
    ("Open a folder as a project", "把文件夹作为项目打开"),
    ("Loading chats…", "正在加载对话…"),
    ("No chats yet.", "还没有对话。"),
    ("Your Codex conversations show here.", "你的 Codex 对话会显示在这里。"),
    ("In messages", "消息中"),
    ("API key", "API key"),
    ("Finish signing in in your browser…", "请在浏览器中完成登录…"),
    ("Goal complete", "目标已完成"),
    ("Usage limit reached. It resets soon; see Usage.", "已达用量上限，很快会重置；详见用量。"),
    ("Signed in to ChatGPT", "已登录 ChatGPT"),
    ("Sign-in failed: {error}", "登录失败：{error}"),
    ("unknown error", "未知错误"),
    ("Conversation compacted", "对话已压缩"),
    ("{reason}. Restart it from the sidebar.", "{reason}。请从侧边栏重启。"),
    ("Could not open this chat: {error}", "无法打开这个对话：{error}"),
    ("Branched into a new chat", "已分支到新对话"),
    ("Could not branch: {error}", "无法分支：{error}"),
    ("Codex is still starting this turn; try again in a moment.", "Codex 还在启动这一轮，请稍后再试。"),
    ("Could not add that to the running turn: {error}", "无法补充到正在进行的这一轮：{error}"),
    ("Could not edit the message: {error}", "无法编辑消息：{error}"),
    ("Start a chat first, then give it a goal.", "请先开始一个对话，再设定目标。"),
    ("Goal set. Codex keeps working toward it.", "目标已设定，Codex 会持续推进。"),
    ("Could not set the goal: {error}", "无法设定目标：{error}"),
    ("Memory on for this chat", "已为这个对话开启记忆"),
    ("Memory off for this chat", "已为这个对话关闭记忆"),
    ("Finish signing in in your browser.", "请在浏览器中完成登录。"),
    ("Could not start sign-in: {error}", "无法开始登录：{error}"),
    ("Signed out", "已退出登录"),
    ("Could not sign out: {error}", "无法退出登录：{error}"),
    ("Codex is not running.", "Codex 未运行。"),
    ("Could not send: {error}", "发送失败：{error}"),
    ("Could not start a chat: {error}", "无法开始对话：{error}"),
    ("Chat archived", "对话已归档"),
    ("Could not archive: {error}", "无法归档：{error}"),
    ("Could not rename: {error}", "无法重命名：{error}"),
    ("Start a chat first, then compact it.", "请先开始一个对话，再压缩。"),
    ("Compacting the conversation…", "正在压缩对话…"),
    ("Could not compact: {error}", "无法压缩：{error}"),
    ("Code review", "代码审查"),
    ("Could not start a review: {error}", "无法开始审查：{error}"),
    ("Default model", "默认模型"),
    ("Could not change that: {error}", "无法修改：{error}"),
    ("{agent} is not running.", "{agent} 未运行。"),
    ("{agent} is still working; wait for it, or stop it first.", "{agent} 还在工作；请等它完成，或先停止。"),
    ("{agent} is not running, so this chat cannot open yet.", "{agent} 未运行，暂时无法打开这个对话。"),
    ("{agent} does not send past messages; the chat continues from here", "{agent} 不会发送历史消息；对话从这里继续"),
    ("Tailscale discovery is off", "已关闭 Tailscale 发现"),
    ("Codex on {device} stopped", "{device} 上的 Codex 已停止"),
    ("Could not open the project: {error}", "无法打开项目：{error}"),
    ("Could not start the workstream: {error}", "无法启动工作线：{error}"),
    ("It is working; the notices go with its next message.", "它正在工作；公告会随它的下一条消息送达。"),
    ("{stream} changed {path} in {owner}'s scope", "{stream} 改动了 {owner} 负责范围内的 {path}"),
    ("{name} hit an error", "{name} 出错了"),
    ("Start a new chat", "开始新对话"),
    ("Summarize to free context", "概括以腾出上下文"),
    ("Open settings", "打开设置"),
    ("Show or hide the sidebar", "显示或隐藏侧边栏"),
    ("Explain this project", "讲讲这个项目"),
    ("Fix a failing test", "修复一个失败的测试"),
    ("Review my changes", "审查我的改动"),
    ("Plan a feature", "规划一个功能"),
    ("Read only", "只读"),
    ("Ask for changes", "要修改"),
    ("Built in", "内置"),
    ("Checking", "检查中"),
    ("Checks", "检查结果"),
    ("Copy and edit", "复制并编辑"),
    ("Default agent", "默认 agent"),
    ("Drop the problem PDF and its data here, or browse.", "把题目 PDF 和附件数据拖到这里，或点「浏览」选择。"),
    ("Files", "产出文件"),
    ("Fill in {label} first.", "请先填写「{label}」。"),
    ("Finished", "已完成"),
    ("Looks good, go on", "没问题，继续"),
    ("New run", "新建运行"),
    ("No runs yet. Pick a template to start one.", "还没有运行。选一个模板开始。"),
    ("Only checks read this; it is never sent to an agent.", "只用于检查，不会发给 agent。"),
    ("Open its conversation", "打开这一步的对话"),
    ("Redo this step and the ones after it", "重做这一步和后面的步骤"),
    ("Running the steps by themselves is being built. Until then this shows the plan, and you can open a step's prompt, approve stops and ask for changes.", "自动执行各步骤的功能还在开发中。现在可以查看计划和每一步会发给 agent 的指令，也可以确认或要求修改。"),
    ("Skip", "跳过"),
    ("Skipped", "已跳过"),
    ("Steps", "步骤"),
    ("Stops for you", "等你确认"),
    ("Template: {source}", "模板：{source}"),
    ("The paper", "论文"),
    ("This run's files are gone.", "这次运行的文件夹已经不在了。"),
    ("This step stops for you. Look at its files, then let the run go on or say what to change.", "这一步在等你确认。看看它的产出，然后让运行继续，或者写下要修改的地方。"),
    ("Try again with this note", "按这条意见重试"),
    ("Waiting", "等待中"),
    ("Waiting for you", "等你确认"),
    ("Environment", "环境"),
    ("Confirm this template's commands", "确认此模板中的命令"),
    ("found", "已安装"),
    ("missing", "未安装"),
    ("What should change? The agent gets this with the step.", "要改什么？这段话会随这一步一起发给 agent。"),
    ("What the agent is told", "发给 agent 的指令"),
    ("What to start from", "开始需要的材料"),
    ("Workflows", "工作流"),
    ("try {n}", "第 {n} 次"),
    ("under a minute", "不到 1 分钟"),
    ("{h} h {m} min", "{h} 小时 {m} 分"),
    ("{m} min", "{m} 分钟"),
    ("Runs", "运行"),
    ("Templates", "模板"),
    ("Remove from list", "从列表移除"),
    ("Copied to {path}. Changes there take the built-in one's place.", "已复制到 {path}。在那里修改后会替代内置模板。"),
    ("Started {name}.", "已开始「{name}」。"),
    ("Could not start the workflow: {error}", "无法开始工作流：{error}"),
    ("The run gets a folder in {home}. Steps that stop for you wait there; the rest go on by themselves.", "这次运行会在 {home} 下新建一个文件夹。需要你确认的步骤会停下来等你，其余步骤自动进行。"),
    ("{done} of {total} steps · {elapsed}", "{done}/{total} 步 · {elapsed}"),
    ("Overview", "概览"),
    ("Outputs", "输出内容"),
    ("No files made or changed yet.", "还没有新建或修改文件。"),
    ("Subagents", "子智能体"),
    ("No subagents in this conversation.", "这段对话没有启动子智能体。"),
    ("{n} more in Activity", "还有 {n} 个，在「活动」里查看"),
    ("Sources", "来源"),
    ("No tools or web searches used yet.", "还没有用到工具或网页搜索。"),
    ("{n} calls", "{n} 次调用"),
    ("Manage MCP servers", "管理 MCP 服务器"),
    ("Open a conversation to see what it made.", "打开一段对话，查看它的产出。"),
    ("edited a file", "编辑了 1 个文件"),
    ("edited {n} files", "编辑了 {n} 个文件"),
    ("read a file", "读取了 1 个文件"),
    ("read {n} files", "读取了 {n} 个文件"),
    ("searched once", "搜索了 1 次"),
    ("searched {n} times", "搜索了 {n} 次"),
    ("ran a command", "运行了 1 条命令"),
    ("ran {n} commands", "运行了 {n} 条命令"),
    ("used a tool", "调用了 1 个工具"),
    ("used {n} tools", "调用了 {n} 个工具"),
    ("searched the web", "搜索了 1 次网页"),
    ("searched the web {n} times", "搜索了 {n} 次网页"),
    ("started a subagent", "启动了 1 个子智能体"),
    ("started {n} subagents", "启动了 {n} 个子智能体"),
    ("Read", "已读取"),
    ("Reading", "正在读取"),
    ("Searched", "已搜索"),
    ("Searching", "正在搜索"),
    ("Created", "已创建"),
    ("Deleted", "已删除"),
    ("Edited", "已编辑"),
    ("Used", "已调用"),
    ("Subagent", "子智能体"),
    ("A content control", "内容控件"),
    ("An embedded document", "嵌入的文档"),
    ("Custom XML content", "自定义 XML 内容"),
    ("A tracked change", "修订"),
    ("Content this editor keeps as it is", "这里无法编辑的内容"),
    (" — edit it in Word", "，请在 Word 里修改"),
    ("Auto", "自动"),
    ("Full access", "完全访问"),
    ("Reads files. Asks before editing or running commands.", "可读文件。编辑或运行命令前会先询问。"),
    ("Edits and runs commands in the workspace. Asks for more.", "可在工作区内编辑和运行命令。超出范围会询问。"),
    ("Edits anything and runs anything without asking.", "可以不经询问编辑任何文件、运行任何命令。"),
    ("Paused", "已暂停"),
    ("Blocked", "受阻"),
    ("Usage limit reached", "已达用量上限"),
    ("Budget used up", "预算已用完"),
    ("Complete", "已完成"),
    ("starting", "启动中"),
    ("not running", "未运行"),
    ("not installed", "未安装"),
    ("offline", "离线"),
    ("Codex exited", "Codex 已退出"),
    ("minimal", "最低"),
    ("low", "低"),
    ("medium", "中"),
    ("high", "高"),
    ("xhigh", "超高"),
    ("none", "无"),
    (
        "Multi-device sync is off: {error}",
        "多设备同步未启动：{error}",
    ),
    (
        "Could not read this conversation from {device}: {error}",
        "无法从 {device} 读取此对话：{error}",
    ),
    ("Already answered on {device}.", "已在 {device} 上答复。"),
    (
        "Could not deliver a turn from another computer: {error}",
        "无法执行另一台电脑发来的消息：{error}",
    ),
    (
        "Could not answer on {device}: {error}",
        "无法在 {device} 上回答：{error}",
    ),
    (
        "Could not stop the turn on {device}: {error}",
        "无法在 {device} 上停止本轮：{error}",
    ),
    ("Move to this computer", "移到此电脑"),
    ("Start a workflow", "开始工作流"),
    (
        "Moving the conversation from {device}…",
        "正在从 {device} 移来对话…",
    ),
    (
        "Could not move the conversation: {error}",
        "无法移动对话：{error}",
    ),
    (
        "Moved the conversation from {device}.",
        "已从 {device} 移来对话。",
    ),
    (
        "The files moved, but Codex could not reopen the session; its history is shown read-only.",
        "文件已迁来，但 Codex 无法重新打开会话；历史将以只读方式显示。",
    ),
    (
        "The other computer could not find this conversation's Codex session file. Open the conversation there once, then try again.",
        "另一台电脑找不到这段对话的 Codex 会话文件。请先在那台电脑上打开一次，再重试。",
    ),
    (
        "Stop the running turn before moving the conversation.",
        "请先在原电脑停止正在运行的回答，再移动对话。",
    ),
    (
        "This conversation's folder is not a Git project, so it cannot be moved.",
        "这段对话所在的文件夹不是 Git 项目，无法移动。",
    ),
    (
        "This computer has no checkout of the conversation's project. Clone it here first.",
        "这台电脑没有该项目的代码副本。请先克隆项目，再移动对话。",
    ),
    (
        "The conversation's session or working tree is larger than 32 MB.",
        "这段对话的会话文件或工作区超过 32 MB，无法移动。",
    ),
    (
        "Moving an ACP conversation is not implemented yet.",
        "暂不支持移动 ACP 对话。",
    ),
    (
        "The other computer no longer has this conversation.",
        "另一台电脑上已经没有这段对话。",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_fill_in_either_language() {
        CHINESE.store(false, Ordering::Relaxed);
        assert_eq!(tf("{n} new", &[("n", &3)]), "3 new");
        assert_eq!(
            t("A string nobody translated"),
            "A string nobody translated"
        );
    }

    /// Every `t("…")` and `tf("…"` in the app's source has a Chinese entry,
    /// and placeholders match.
    #[test]
    fn every_string_has_chinese() {
        let sources = [
            include_str!("main.rs"),
            include_str!("agents.rs"),
            include_str!("workspace.rs"),
            include_str!("workspace_sync.rs"),
            include_str!("workspace_acp.rs"),
            include_str!("workspace_devices.rs"),
            include_str!("workspace_project.rs"),
            include_str!("workspace_worktrees.rs"),
            include_str!("workspace_checkpoints.rs"),
            include_str!("workspace_chat_trees.rs"),
            include_str!("ui/tools.rs"),
            include_str!("ui/run.rs"),
            include_str!("ui/preview.rs"),
            include_str!("ui/document.rs"),
            include_str!("ui/steps.rs"),
            include_str!("ui/overview.rs"),
            include_str!("ui/workflow.rs"),
            include_str!("workspace_workflows.rs"),
            include_str!("docmap.rs"),
            include_str!("export.rs"),
            include_str!("schedule.rs"),
            include_str!("workspace_schedules.rs"),
            include_str!("workspace_updates.rs"),
            include_str!("ui/mod.rs"),
            include_str!("ui/activity.rs"),
            include_str!("ui/changes.rs"),
            include_str!("ui/extras.rs"),
            include_str!("ui/chat.rs"),
            include_str!("ui/composer.rs"),
            include_str!("ui/context.rs"),
            include_str!("ui/devices.rs"),
            include_str!("ui/project.rs"),
            include_str!("ui/rows.rs"),
            include_str!("ui/sidebar.rs"),
        ];
        let table = table();
        let mut missing = Vec::new();
        for source in sources {
            for marker in ["t(\"", "tf(\""] {
                for (at, _) in source.match_indices(marker) {
                    // Only whole calls: `t(` not preceded by a letter.
                    let before = source[..at].chars().last();
                    if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                        continue;
                    }
                    let start = at + marker.len();
                    let end = start + source[start..].find('"').unwrap();
                    let english = source[start..end].replace("\\n", "\n");
                    let english = english.as_str();
                    match table.get(english) {
                        None => missing.push(english.to_string()),
                        Some(chinese) => {
                            let names = |s: &str| {
                                let mut names: Vec<String> = s
                                    .split('{')
                                    .skip(1)
                                    .filter_map(|p| p.split_once('}').map(|(n, _)| n.to_string()))
                                    .collect();
                                names.sort();
                                names
                            };
                            assert_eq!(
                                names(english),
                                names(chinese),
                                "placeholders of {english:?}"
                            );
                        }
                    }
                }
            }
        }
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "no Chinese for {missing:#?}");
    }

    /// Strings looked up at run time rather than written as `t("…")`.
    #[test]
    fn run_time_strings_have_chinese() {
        let dynamic = [
            "Start a new chat",
            "Review uncommitted changes",
            "Summarize to free context",
            "Attach images or files",
            "Open settings",
            "Show or hide the sidebar",
            "Explain this project",
            "Fix a failing test",
            "Review my changes",
            "Plan a feature",
            "starting",
            "not running",
            "not installed",
            "offline",
            "Codex exited",
            "low",
            "medium",
            "high",
            "xhigh",
        ];
        let table = table();
        let mut missing: Vec<&str> = dynamic
            .into_iter()
            .filter(|k| !table.contains_key(k))
            .collect();
        for mode in codex_client::AccessMode::ALL {
            missing.extend(
                [mode.label(), mode.description()]
                    .into_iter()
                    .filter(|k| !table.contains_key(k)),
            );
        }
        for status in [
            atrium_core::GoalStatus::Active,
            atrium_core::GoalStatus::Paused,
            atrium_core::GoalStatus::Blocked,
            atrium_core::GoalStatus::UsageLimited,
            atrium_core::GoalStatus::BudgetLimited,
            atrium_core::GoalStatus::Complete,
        ] {
            if !table.contains_key(status.label()) {
                missing.push(status.label());
            }
        }
        assert!(missing.is_empty(), "no Chinese for {missing:?}");
    }

    #[test]
    fn no_entry_repeats() {
        let mut keys: Vec<&str> = ZH.iter().map(|(en, _)| *en).collect();
        let count = keys.len();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), count, "a key is listed twice");
    }
}
