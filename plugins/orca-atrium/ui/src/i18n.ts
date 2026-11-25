// Every UI string, in Simplified Chinese and English. `t('key', { n: 3 })`.
import type { Lang, AgentState, StepStatus, TaskStatus, RunState, NeedKind } from '../../worker/src/protocol'
import { locale } from './orca'

const S = {
  // app
  atrium: ['Atrium', 'Atrium'],
  loading: ['正在加载…', 'Loading…'],
  retry: ['重试', 'Try again'],
  loadFailed: ['加载失败：{error}', 'Could not load: {error}'],
  workerDown: ['连不上 Atrium 后台。它会在下次操作时自动启动；也可以点「重试」。', 'Atrium’s worker is not answering. It starts again on the next action, or press Try again.'],
  settings: ['设置', 'Settings'],
  close: ['关闭', 'Close'],
  fullscreen: ['全屏展开', 'Fullscreen'],
  exitFullscreen: ['退出全屏', 'Exit fullscreen'],
  fullscreenUnsupported: ['当前窗口不支持全屏', 'Fullscreen is not available here'],
  cancel: ['取消', 'Cancel'],
  save: ['保存', 'Save'],
  saved: ['已保存', 'Saved'],
  saving: ['正在保存…', 'Saving…'],
  open: ['打开', 'Open'],
  more: ['更多', 'More'],
  remove: ['移除', 'Remove'],
  add: ['添加', 'Add'],
  done: ['完成', 'Done'],
  none: ['无', 'None'],
  justNow: ['刚刚', 'just now'],
  minAgo: ['{n} 分钟前', '{n} min ago'],
  hourAgo: ['{n} 小时前', '{n} h ago'],
  dayAgo: ['{n} 天前', '{n} d ago'],
  underMinute: ['不到 1 分钟', 'under a minute'],
  mins: ['{m} 分钟', '{m} min'],
  hoursMins: ['{h} 小时 {m} 分', '{h} h {m} min'],
  resetsIn: ['约 {t} 后重置', 'back in about {t}'],

  // sidebar
  needsYou: ['需要你', 'Needs you'],
  needsCount: ['{n} 条需要你', '{n} need you'],
  runningCount: ['{n} 个运行中', '{n} running'],
  allClear: ['没有需要你处理的事', 'Nothing needs you'],
  allClearHint: ['agent 卡住、提问或到了确认点时，会出现在这里。', 'Agents show up here when they get stuck, ask something or reach a stop.'],
  projects: ['项目', 'Projects'],
  runs: ['运行', 'Runs'],
  noProjects: ['还没有项目', 'No projects yet'],
  noProjectsHint: ['项目是一个文件夹：主控拆分目标，负责人分头推进，子 agent 完成具体任务。', 'A project is a folder: the controller splits the goal, leads own the parts, workers do the tasks.'],
  makeProject: ['把当前文件夹设为 Atrium 项目', 'Make this folder an Atrium project'],
  noRuns: ['还没有运行', 'No runs yet'],
  noRunsHint: ['工作流按步骤推进，需要你确认的地方会停下来等你。', 'A workflow goes step by step and stops where it needs you.'],
  startWorkflow: ['开始工作流', 'Start a workflow'],
  openBoard: ['打开看板', 'Open board'],
  openRun: ['打开运行', 'Open run'],
  autopilot: ['无人值守', 'Autopilot'],
  autopilotOn: ['无人值守已开启：Atrium 会按计划自动启动 agent', 'Autopilot on: Atrium starts agents as the plan needs them'],
  autopilotOff: ['无人值守已关闭：agent 只在你启动时工作', 'Autopilot off: agents work only when you start them'],
  milestoneProgress: ['任务 {done}/{total}', '{done}/{total} tasks'],
  stepProgress: ['{done}/{total} 步', '{done} of {total} steps'],
  rolesLive: ['{working} 个在工作 · 共 {total} 个角色', '{working} working of {total} roles'],
  projectNeeds: ['{n} 条待处理', '{n} to handle'],
  elapsed: ['已用 {t}', '{t} so far'],
  nowAt: ['当前：{step}', 'Now: {step}'],

  // need kinds
  need_gate: ['等你确认', 'Waiting for you'],
  need_question: ['有问题问你', 'Asks you'],
  need_stalled: ['停住了', 'Stalled'],
  need_blocked: ['受阻', 'Blocked'],
  need_brief: ['请确认共识', 'Confirm the brief'],
  need_quota: ['已达用量上限', 'Usage limit'],
  need_error: ['出错了', 'Error'],
  approve: ['没问题，继续', 'Looks good, go on'],
  sendBack: ['要修改', 'Send back'],
  sendBackTo: ['可退回到…', 'Send back to…'],
  sendBackTitle: ['退回到「{step}」', 'Send back to {step}'],
  sendBackNote: ['要改什么？这段话会随这一步一起发给 agent。', 'What should change? The agent gets this with the step.'],
  sendBackConfirm: ['按这条意见重试', 'Send back'],
  sentBack: ['已退回到「{step}」', 'Sent back to {step}'],
  approved: ['已继续', 'Going on'],
  chosen: ['你选了：{choice}', 'You chose {choice}'],
  reply: ['回复', 'Reply'],
  replyPlaceholder: ['写下你的回答…', 'Write your answer…'],
  replied: ['已回复', 'Replied'],
  nudge: ['催一下', 'Nudge'],
  nudged: ['已提醒它继续', 'Nudged'],
  openAgent: ['打开对话', 'Open conversation'],
  dismiss: ['关闭', 'Dismiss'],
  fromRun: ['来自运行', 'From a run'],
  fromProject: ['来自项目', 'From a project'],
  stalledFor: ['已停住 {t}', 'stalled for {t}'],

  // agent states
  st_working: ['工作中', 'Working'],
  st_waiting: ['等待中', 'Waiting'],
  st_blocked: ['受阻', 'Blocked'],
  st_done: ['完成', 'Done'],
  st_idle: ['空闲', 'Idle'],
  st_stalled: ['停住了', 'Stalled'],
  st_starting: ['启动中', 'Starting'],
  st_unbound: ['未启动', 'Not started'],

  // roles
  controller: ['主控', 'Controller'],
  lead: ['负责人', 'Lead'],
  worker: ['子 agent', 'Worker'],
  leadOf: ['负责人 · {name}', 'Lead · {name}'],
  workerOf: ['子 agent · {name}', 'Worker · {name}'],
  standIn: ['替补', 'Stand-in'],
  standingInFor: ['替补 {member}', 'Covering for {member}'],
  quota: ['用量', 'Usage'],
  quotaUsed: ['已用 {p}%', '{p}% used'],
  quotaLimited: ['已达用量上限', 'Limit reached'],
  noTask: ['没有在做的任务', 'No task right now'],
  unassigned: ['未分配', 'Unassigned'],

  // board
  tabCommand: ['指挥台', 'Command'],
  tabTasks: ['任务', 'Tasks'],
  tabTeam: ['团队', 'Team'],
  brief: ['共识', 'Brief'],
  showBrief: ['展开共识', 'Show brief'],
  hideBrief: ['收起', 'Hide'],
  addNotice: ['发公告', 'Post notice'],
  noticePlaceholder: ['发给所有 agent 的公告，比如：接口 v2 已合并，请改用新字段。', 'A notice to every agent, such as: API v2 is merged, use the new fields.'],
  noticePosted: ['公告已发出', 'Notice posted'],
  post: ['发出', 'Post'],
  orgChart: ['组织', 'Organisation'],
  milestones: ['里程碑', 'Milestones'],
  noMilestones: ['主控写好计划后，里程碑会出现在这里。', 'Milestones appear here once the controller writes the plan.'],
  activity: ['活动', 'Activity'],
  noActivity: ['还没有动态。agent 交接任务、发公告时会出现在这里。', 'Nothing yet. Handoffs and notices show up here.'],
  focusTerminal: ['打开对话', 'Open conversation'],
  startRole: ['开始', 'Start'],
  stopRole: ['停止', 'Stop'],
  reassign: ['把当前任务交给…', 'Hand its task to…'],
  roleDetails: ['角色详情', 'Role details'],
  member: ['成员', 'Member'],
  agentKind: ['Agent', 'Agent'],
  model: ['模型', 'Model'],
  currentTask: ['当前任务', 'Current task'],
  tasksOwned: ['负责的任务', 'Its tasks'],
  stopConfirm: ['停止后它手上的任务会回到待办。', 'Its task goes back to To do.'],
  roleStarted: ['已启动 {role}', 'Started {role}'],
  roleStopped: ['已停止 {role}', 'Stopped {role}'],
  ts_todo: ['待办', 'To do'],
  ts_ready: ['待领', 'Ready'],
  ts_in_progress: ['进行中', 'In progress'],
  ts_review: ['待审阅', 'Review'],
  ts_blocked: ['受阻', 'Blocked'],
  ts_done: ['已完成', 'Done'],
  swimlanes: ['按里程碑分组', 'Group by milestone'],
  noMilestone: ['未归入里程碑', 'No milestone'],
  newTask: ['新任务', 'New task'],
  newTaskPlaceholder: ['任务标题，回车创建', 'Task title, Enter to add'],
  moveTo: ['移到…', 'Move to…'],
  moved: ['已移到「{status}」', 'Moved to {status}'],
  pool: ['任务池', 'Pool'],
  poolHint: ['放进任务池，由空闲的子 agent 领取', 'In the pool: any free worker can take it'],
  dropHere: ['拖到这里', 'Drop here'],
  emptyColumn: ['空', 'Empty'],
  taskFile: ['任务文件', 'Task file'],
  owner: ['负责', 'Owner'],

  // team
  teamIntro: ['主控按这些备注决定每个任务交给谁。某个模型达到用量上限时，由它的替补按顺序接手，重置后换回来。', 'The controller picks who does each task from these notes. When one hits its usage limit, its stand-ins take over in order until it resets.'],
  addMember: ['添加成员', 'Add member'],
  memberName: ['名称', 'Name'],
  effort: ['推理强度', 'Effort'],
  effortDefault: ['默认', 'Default'],
  folders: ['可用文件夹', 'Folders'],
  foldersPlaceholder: ['比如 app/，回车添加', 'Such as app/, Enter to add'],
  notes: ['备注', 'Notes'],
  notesPlaceholder: ['它擅长和不擅长什么，写给主控看', 'What it is good and bad at, for the controller'],
  standIns: ['替补顺序', 'Stand-ins, in order'],
  noStandIn: ['没有替补', 'No stand-in'],
  addStandIn: ['添加替补', 'Add stand-in'],
  moveUp: ['上移', 'Move up'],
  moveDown: ['下移', 'Move down'],
  dragToOrder: ['拖动调整顺序', 'Drag to reorder'],
  unsavedChanges: ['有未保存的修改', 'Unsaved changes'],
  discard: ['放弃修改', 'Discard'],
  teamSaved: ['团队已保存', 'Team saved'],
  errDuplicateName: ['名称「{name}」重复了', 'The name {name} is used twice'],
  errEmptyName: ['每个成员都要有名称', 'Every member needs a name'],
  errSelfStandIn: ['「{name}」不能做自己的替补', '{name} cannot stand in for itself'],
  errUnknownStandIn: ['替补「{name}」不在团队里', 'Stand-in {name} is not on the team'],
  cannotSave: ['还不能保存', 'Cannot be saved yet'],
  removeMember: ['移除成员', 'Remove member'],
  quotaNow: ['当前用量', 'Usage now'],

  // run
  rs_running: ['运行中', 'Running'],
  rs_waiting: ['等你确认', 'Waiting for you'],
  rs_paused: ['已暂停', 'Paused'],
  rs_done: ['已完成', 'Finished'],
  rs_failed: ['失败', 'Failed'],
  rs_cancelled: ['已取消', 'Cancelled'],
  ss_pending: ['未开始', 'Not started'],
  ss_running: ['进行中', 'Running'],
  ss_checking: ['检查中', 'Checking'],
  ss_waiting: ['等你确认', 'Waiting for you'],
  ss_passed: ['已通过', 'Passed'],
  ss_failed: ['失败', 'Failed'],
  ss_skipped: ['已跳过', 'Skipped'],
  ss_sent_back: ['已退回', 'Sent back'],
  pause: ['暂停', 'Pause'],
  resume: ['继续', 'Resume'],
  cancelRun: ['取消运行', 'Cancel run'],
  cancelRunConfirm: ['确定取消这次运行吗？已完成的产出会保留。', 'Cancel this run? Finished files stay.'],
  keepRunning: ['继续运行', 'Keep running'],
  inputs: ['开始时的材料', 'What it started from'],
  steps: ['步骤', 'Steps'],
  attemptN: ['第 {n} 次', 'try {n}'],
  attempts: ['尝试记录', 'Attempts'],
  report: ['报告', 'Report'],
  noReport: ['这一步还没有写报告。', 'No report from this step yet.'],
  checks: ['检查结果', 'Checks'],
  noChecks: ['这一步没有检查项。', 'This step has no checks.'],
  checkPending: ['未检查', 'Not checked'],
  showLog: ['查看日志', 'Show log'],
  hideLog: ['收起日志', 'Hide log'],
  prompt: ['发给 agent 的指令', 'What the agent is told'],
  experience: ['经验', 'Lessons'],
  experienceHint: ['从以往运行中保留的经验，新运行会带上。', 'Lessons kept from earlier runs; new runs start with them.'],
  noExperience: ['还没有保留的经验。', 'No lessons kept yet.'],
  decisions: ['决策记录', 'Decisions'],
  noDecisions: ['还没有决策。需要你确认的步骤会记在这里。', 'No decisions yet. Stops that need you are logged here.'],
  retryStep: ['重做这一步', 'Redo this step'],
  retryItem: ['重做 {item}', 'Redo {item}'],
  retried: ['已重新开始「{step}」', 'Started {step} again'],
  gateWaiting: ['这一步在等你确认。看看它的产出，然后让运行继续，或者退回到要修改的那一步。', 'This step stops for you. Look at its files, then let the run go on or send it back.'],
  pickChoice: ['选一个方案', 'Pick an option'],
  items: ['逐条进行 · {done}/{total}', 'Per item · {done}/{total}'],
  sendBackEdge: ['退回', 'sent back'],
  selectStep: ['选择一个步骤查看详情', 'Pick a step to see its details'],
  stepKind_agent: ['agent 步骤', 'Agent step'],
  stepKind_gate: ['确认点', 'Stop for you'],
  stepKind_each: ['逐条步骤', 'Per-item step'],
  runPaused: ['已暂停', 'Paused'],
  runResumed: ['已继续运行', 'Resumed'],
  runCancelled: ['已取消运行', 'Run cancelled'],
  outcome_passed: ['通过', 'Passed'],
  outcome_failed: ['失败', 'Failed'],
  outcome_sent_back: ['被退回', 'Sent back'],
  outcome_stalled: ['停住了', 'Stalled'],
  inProgress: ['进行中', 'In progress'],
  sendBackTargets: ['可退回', 'Can send back to'],
  runFinished: ['运行已完成', 'Run finished'],
  legend: ['图例', 'Legend'],
  noteLabel: ['备注：{note}', 'Note: {note}'],

  // template editor
  tplSteps: ['步骤图', 'Steps'],
  tplSource: ['TOML', 'TOML'],
  tplValid: ['没有错误', 'No problems'],
  tplErrors: ['{n} 个问题', '{n} problems'],
  tplLine: ['第 {n} 行', 'line {n}'],
  tplAddStep: ['添加步骤', 'Add a step'],
  tplDeleteStep: ['删除步骤', 'Delete step'],
  tplStepTitle: ['标题', 'Title'],
  tplStepId: ['标识', 'Id'],
  tplSummary: ['一句话说明', 'One line on what it is for'],
  tplAgent: ['由谁来做', 'Who does it'],
  tplAgentDefault: ['按模板默认', 'As the template says'],
  tplNeeds: ['在这些步骤之后开始', 'Starts after'],
  tplNeedsPrev: ['上一步', 'The step before'],
  tplNeedsNone: ['不等：一开始就做', 'Nothing: starts at once'],
  tplGate: ['停下来等你确认', 'Stops for you'],
  tplEach: ['按清单逐条进行', 'Once per item of a list'],
  tplEachFrom: ['清单文件', 'List file'],
  tplEachMax: ['最多几条', 'At most'],
  tplFixes: ['检查不通过时，把问题退回给', 'When its checks fail, send the faults back to'],
  tplAttempts: ['尝试次数', 'Tries'],
  tplPrompt: ['发给 agent 的指令', 'What the agent is told'],
  tplOutputs: ['必须产出的文件', 'Files it must leave'],
  tplChecks: ['{n} 项检查', '{n} checks'],
  tplOptional: ['可选步骤', 'Optional'],
  tplWhen: ['仅当', 'Only when'],
  tplConnectHint: ['从步骤右侧的圆点拖到另一个步骤，就能设定先后。点连线可以删除。', 'Drag from a step’s right dot to another step to set the order. Click a line to remove it.'],
  tplRemoveEdge: ['删除这条依赖', 'Remove this link'],
  tplReadOnly: ['只读：这个文件不能在这里修改。', 'Read only: this file cannot be changed here.'],
  tplParseError: ['TOML 解析失败：{error}', 'TOML could not be read: {error}'],
  tplNoSteps: ['还没有步骤', 'No steps yet'],
  tplNoStepsHint: ['每一步是一段 agent 对话。产出文件齐全、检查全部通过，这一步才算完成。', 'Each step is one agent conversation. It is done when its files exist and its checks pass.'],
  tplSelectStep: ['选中一个步骤来编辑', 'Pick a step to edit it'],
  tplNewStepTitle: ['新步骤', 'New step'],
  tplUndo: ['撤销', 'Undo'],
  tplRedo: ['重做', 'Redo'],
  tplConflict: ['文件在外部被修改，已载入最新内容。', 'The file changed outside; loaded the latest.'],
  tplInputs: ['{n} 项材料', '{n} inputs'],
  tplCycle: ['步骤之间形成了循环依赖：{steps}', 'Steps depend on each other in a loop: {steps}'],
  tplUnknownNeed: ['「{step}」依赖的步骤「{need}」不存在', '{step} needs {need}, which does not exist'],
  tplDupId: ['步骤标识「{id}」重复了', 'Step id {id} is used twice'],
  tplLayout: ['重新排列', 'Tidy layout'],

  // board extras
  after: ['在这些之后', 'After'],
  autopilotOnShort: ['已开启：按计划启动 agent', 'On: agents start as planned'],
  autopilotOffShort: ['已关闭：由你来启动 agent', 'Off: you start agents'],
  briefEmpty: ['还没有写共识。主控第一次启动时，会和你确认目标。', 'No brief yet. The controller confirms the goal with you on its first start.'],
  orgEmpty: ['还没有角色。启动主控后，它会把团队搭起来。', 'No roles yet. They appear once the controller sets the team up.'],
  linkError: ['这个文件不是 Atrium 看板的链接，没法打开。', 'This file is not an Atrium link, so it cannot be opened.'],
  linkWrong: ['这个文件指向的不是项目。', 'This file does not point to a project.'],
  openingLink: ['正在打开…', 'Opening…'],
  noOwnedTasks: ['没有分到任务。', 'No tasks yet.'],
  tasksTotal: ['{n} 个任务', '{n} tasks'],
  handoffLive: ['正在交接', 'handing work down'],
  lane: ['{title} · {done}/{total}', '{title} · {done}/{total}'],
  newMember: ['新成员', 'New member'],
  removeMemberConfirm: ['移除「{name}」？它在别人替补名单里的名字也会去掉。', 'Remove {name}? It also leaves other stand-in lists.'],
  coveringNow: ['正在替 {member} 工作', 'Working in for {member}'],
  teamEmpty: ['还没有成员', 'No members yet'],
  teamEmptyHint: ['添加成员后，主控才知道有哪些 agent 可用。', 'Add members so the controller knows which agents it can use.'],
  noMatchStandIn: ['没有可选的成员了', 'No more members to add'],
  statusOf: ['状态', 'Status'],
  usageResets: ['用量已用 {p}%，{reset}', '{p}% used, {reset}'],
  folderAdd: ['添加', 'Add'],
  agentOther: ['其他', 'Other'],
  stepsCount: ['{n} 个步骤', '{n} steps'],
  noStepSelected: ['点一个步骤，看它的报告、检查和历史。', 'Pick a step to see its report, checks and history.'],
  runOverview: ['这次运行', 'This run'],
  zoomFit: ['适应窗口', 'Fit to window'],
  zoomIn: ['放大', 'Zoom in'],
  zoomOut: ['缩小', 'Zoom out'],
  acting: ['你的决定', 'Your decision'],
  decisionNote: ['给 agent 的补充说明（可选）', 'A note for the agent (optional)'],
  gateWho: ['需要你：{title}', 'Needs you: {title}'],
  needsStep: ['在这些步骤之后：{steps}', 'After: {steps}'],
  stepStart: ['开始于 {t}', 'started {t}'],
  openStepAgent: ['打开这一步的对话', 'Open this step’s conversation'],
  confirmCancel: ['取消运行', 'Cancel run'],
  latest: ['最新', 'Latest'],
  loadingRun: ['正在读取运行…', 'Reading the run…'],
  runLinkWrong: ['这个文件指向的不是运行。', 'This file does not point to a run.'],
  decidedAt: ['{t} 你选了「{choice}」', 'You chose {choice} at {t}'],
  decidedBack: ['{t} 退回到「{step}」', 'Sent back to {step} at {t}'],
  attemptsEmpty: ['还没有开始。', 'Not started.'],
  checkOk: ['通过', 'Passed'],
  checkFail: ['未通过', 'Failed'],
  waitingFor: ['在等 {steps}', 'Waiting for {steps}'],
  sendBackCount: ['退回 {n} 次', 'sent back {n}×'],
  tplFields: ['步骤设置', 'Step settings'],
  tplChecksList: ['检查项', 'Checks'],
  tplChecksHint: ['检查项请在右侧 TOML 里编辑。', 'Edit checks in the TOML on the right.'],
  tplOutputsPlaceholder: ['比如 reports/ANALYSIS.md，回车添加', 'Such as reports/ANALYSIS.md, Enter to add'],
  tplFixesHint: ['格式：步骤标识，检查没过时回到那一步重做', 'Step ids; when checks fail the work goes back there'],
  tplAddNeed: ['添加依赖…', 'Add a dependency…'],
  tplMetadata: ['模板信息', 'Template'],
  tplSelectedGate: ['停下来等你确认的步骤，可以把工作退回到 fixes 里的步骤。', 'A stop for you; work can go back to the steps in fixes.'],
  tplNameLabel: ['名称', 'Name'],
  tplDescLabel: ['说明', 'Description'],
  tplDelConfirm: ['删除「{title}」？指向它的依赖也会去掉。', 'Delete {title}? Links to it go too.'],
  tplFormat: ['按顺序', 'In order'],
  tplLoading: ['正在读取模板…', 'Reading the template…'],
  tplSaved: ['已同步到文件', 'Synced to the file'],
  tplOpenFailed: ['读不到这个文件。', 'Could not read this file.'],
  tplEachMaxHint: ['最多处理清单里的前几条', 'Handle at most this many items'],
  tplStepsHeading: ['{n} 个步骤', '{n} steps'],
  tplInputsHeading: ['材料', 'Inputs'],
  tplDrag: ['拖动圆点连线', 'Drag the dot to connect'],
} as const satisfies Record<string, readonly [string, string]>

export type StringKey = keyof typeof S

let current: Lang = pickLang()

function pickLang(): Lang {
  try {
    return /^zh/i.test(locale()) ? 'zh' : 'en'
  } catch {
    return 'en'
  }
}

export function setLang(lang: Lang | undefined | null) {
  if (lang === 'zh' || lang === 'en') current = lang
  document.documentElement.lang = current === 'zh' ? 'zh-CN' : 'en'
}

export function lang(): Lang {
  return current
}

export function t(key: StringKey, vars?: Record<string, string | number>): string {
  const pair = S[key]
  let s: string = pair ? pair[current === 'zh' ? 0 : 1] : key
  if (vars) for (const k in vars) s = s.split(`{${k}}`).join(String(vars[k]))
  return s
}

export const agentStateLabel = (s: AgentState | undefined) => (s ? t(`st_${s}` as StringKey) : t('st_unbound'))
export const stepStatusLabel = (s: StepStatus) => t(`ss_${s}` as StringKey)
export const taskStatusLabel = (s: TaskStatus) => t(`ts_${s}` as StringKey)
export const runStateLabel = (s: RunState) => t(`rs_${s}` as StringKey)
export const outcomeLabel = (o: 'passed' | 'failed' | 'sent_back' | 'stalled') => t(`outcome_${o}` as StringKey)
export const needKindLabel = (k: NeedKind) => t(`need_${k}` as StringKey)

/** "3 分钟前" style relative time. */
export function ago(at: number, now = Date.now()): string {
  const m = Math.floor((now - at) / 60000)
  if (m < 1) return t('justNow')
  if (m < 60) return t('minAgo', { n: m })
  const h = Math.floor(m / 60)
  if (h < 24) return t('hourAgo', { n: h })
  return t('dayAgo', { n: Math.floor(h / 24) })
}

/** Elapsed duration, "1 小时 12 分". */
export function duration(ms: number): string {
  const m = Math.floor(ms / 60000)
  if (m < 1) return t('underMinute')
  if (m < 60) return t('mins', { m })
  return t('hoursMins', { h: Math.floor(m / 60), m: m % 60 })
}

export function clock(at: number): string {
  const d = new Date(at)
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
}
