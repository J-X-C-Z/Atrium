// Realistic Chinese fixtures for the mock host: a modelling-contest project and a CUMCM run mid-way.
import type { LiveAgent, Member, Milestone, NeedItem, Notice, ProjectView, RoleNode, RunView, StepView, Task, TemplateSummary } from '../../../worker/src/protocol'

const MIN = 60000
export const now = Date.now()
const ago = (m: number) => now - m * MIN

export const ROOT = '/home/user/cumcm-2026'
export const RUN_DIR = '/home/user/cumcm-2026/.atrium/runs/20261125-1002'
export const ROOT2 = '/home/user/pay-gateway'
export const RUN_DIR2 = '/home/user/pay-gateway/.atrium/runs/20261125-0915'

export const team: Member[] = [
  { name: 'Claude Opus', agent: 'claude', model: 'opus-4.5', effort: 'high', folders: ['./'], notes: '统筹和评审最稳，读题时会主动追问歧义。适合做主控。', standIns: ['Claude Sonnet'] },
  { name: 'Claude Sonnet', agent: 'claude', model: 'sonnet-4.5', effort: 'medium', folders: ['paper/', 'reports/'], notes: '写作流畅，论文和报告交给它。不擅长长时间的数值调参。', standIns: ['Gemini'] },
  { name: 'Codex', agent: 'codex', model: 'gpt-5.1-codex', effort: 'high', folders: ['code/', 'results/'], notes: '写代码、跑实验、清洗数据最稳；额度用得快。', standIns: ['Gemini', 'DeepSeek'] },
  { name: 'Gemini', agent: 'gemini', model: '2.5-pro', folders: ['code/', 'data/'], notes: '上下文很长，适合读文献和大表格。', standIns: ['Claude Sonnet'] },
  { name: 'DeepSeek', agent: 'deepseek', model: 'r1', effort: 'high', folders: ['code/model/'], notes: '数学推导和求解思路强，不适合写长文。', standIns: [] },
  { name: 'Claude Haiku', agent: 'claude', model: 'haiku-4.5', effort: 'low', folders: ['figures/'], notes: '便宜又快，画图和排版检查。', standIns: ['Claude Sonnet'] },
]

const leaf = (id: string, role: RoleNode['role'], o: Partial<RoleNode>): RoleNode => ({ id, role, children: [], ...o })

export const org: RoleNode = leaf('controller', { kind: 'controller' }, {
  member: 'Claude Opus', agent: 'claude', model: 'opus-4.5', live: 'working', terminalId: 'term-ctl', currentTask: '根据中期结果更新里程碑，安排论文提纲',
  quota: { used: 0.34 },
  children: [
    leaf('lead:建模', { kind: 'lead', name: '建模' }, {
      member: 'Claude Sonnet', agent: 'claude', model: 'sonnet-4.5', live: 'working', terminalId: 'term-l1', currentTask: '审核问题三的灵敏度分析', quota: { used: 0.52 },
      children: [
        leaf('worker:建模/数据处理', { kind: 'worker', lead: '建模', name: '数据处理' }, { member: 'Gemini', agent: 'gemini', model: '2.5-pro', live: 'working', terminalId: 'term-w1', currentTask: '清洗附件 3 的缺失值并生成 q3.csv', quota: { used: 0.41 }, standingInFor: 'Codex' }),
        leaf('worker:建模/模型求解', { kind: 'worker', lead: '建模', name: '模型求解' }, { member: 'DeepSeek', agent: 'deepseek', model: 'r1', live: 'working', terminalId: 'term-w2', currentTask: '问题四：整数规划的割平面加速', quota: { used: 0.78 } }),
      ],
    }),
    leaf('lead:论文', { kind: 'lead', name: '论文' }, {
      member: 'Claude Sonnet', agent: 'claude', model: 'sonnet-4.5', live: 'stalled', terminalId: 'term-l2', currentTask: '等问题三结果，暂停了 12 分钟', quota: { used: 0.52 },
      children: [
        leaf('worker:论文/正文写作', { kind: 'worker', lead: '论文', name: '正文写作' }, { member: 'Claude Sonnet', agent: 'claude', model: 'sonnet-4.5', live: 'idle', quota: { used: 0.52 } }),
        leaf('worker:论文/图表制作', { kind: 'worker', lead: '论文', name: '图表制作' }, { member: 'Claude Haiku', agent: 'claude', model: 'haiku-4.5', live: 'blocked', terminalId: 'term-w4', currentTask: '图 3 配色待确认', quota: { used: 0.2 } }),
      ],
    }),
  ],
})

export const milestones: Milestone[] = [
  { id: 'm1', title: '审题与方案', phases: [{ id: 'm1p1', title: '读题拆解', done: 3, total: 3 }, { id: 'm1p2', title: '选定路线', done: 2, total: 2 }] },
  { id: 'm2', title: '建模与求解', phases: [{ id: 'm2p1', title: '数据准备', done: 1, total: 2 }, { id: 'm2p2', title: '逐问求解', done: 1, total: 4 }, { id: 'm2p3', title: '稳健性', done: 0, total: 2 }] },
  { id: 'm3', title: '论文成稿', phases: [{ id: 'm3p1', title: '图表', done: 0, total: 2 }, { id: 'm3p2', title: '正文与验收', done: 0, total: 3 }] },
]

const tk = (id: string, title: string, status: Task['status'], o: Partial<Task> = {}): Task => ({ id, title, status, file: `.atrium/tasks/${id}.md`, updatedAt: ago(30), ...o })
export const tasks: Task[] = [
  tk('t1', '通读赛题，拆出四个子问题', 'done', { owner: 'controller', milestone: 'm1', phase: 'm1p1' }),
  tk('t2', '统一变量编号和单位', 'done', { owner: 'lead:建模', milestone: 'm1', phase: 'm1p1' }),
  tk('t3', '扫描附件数据：规模、缺失、异常', 'done', { owner: 'worker:建模/数据处理', milestone: 'm1', phase: 'm1p1' }),
  tk('t4', '比较三条建模路线并做小样试验', 'done', { owner: 'lead:建模', milestone: 'm1', phase: 'm1p2' }),
  tk('t5', '你选定路线 B（机理模型加整数规划）', 'done', { owner: 'controller', milestone: 'm1', phase: 'm1p2' }),
  tk('t6', '清洗附件 1、2 并合并成总表', 'done', { owner: 'worker:建模/数据处理', milestone: 'm2', phase: 'm2p1' }),
  tk('t7', '清洗附件 3 的缺失值，生成 q3.csv', 'in_progress', { owner: 'worker:建模/数据处理', milestone: 'm2', phase: 'm2p1' }),
  tk('t8', '问题一：机理模型建立与求解', 'done', { owner: 'worker:建模/模型求解', milestone: 'm2', phase: 'm2p2' }),
  tk('t9', '问题二：参数估计与拟合检验', 'review', { owner: 'lead:建模', milestone: 'm2', phase: 'm2p2' }),
  tk('t10', '问题三：灵敏度分析', 'in_progress', { owner: 'lead:建模', milestone: 'm2', phase: 'm2p2' }),
  tk('t11', '问题四：整数规划求解与割平面加速', 'in_progress', { owner: 'worker:建模/模型求解', milestone: 'm2', phase: 'm2p2' }),
  tk('t12', '关键参数的稳健性与误差评价', 'todo', { milestone: 'm2', phase: 'm2p3', pool: true }),
  tk('t13', '全部结果图（PDF 矢量，黑白可读）', 'blocked', { owner: 'worker:论文/图表制作', milestone: 'm3', phase: 'm3p1' }),
  tk('t14', '汇总表格：四问结果对照', 'ready', { milestone: 'm3', phase: 'm3p1', pool: true }),
  tk('t15', '摘要与问题重述', 'ready', { owner: 'worker:论文/正文写作', milestone: 'm3', phase: 'm3p2' }),
  tk('t16', '模型的评价与推广', 'todo', { milestone: 'm3', phase: 'm3p2' }),
  tk('t17', '逐项验收：匿名、引用台账、页数', 'todo', { milestone: 'm3', phase: 'm3p2' }),
]

export const notices: Notice[] = [
  { at: ago(6), from: 'lead:建模', text: '问题二已交付审阅：拟合优度 R² = 0.962，残差图在 results/q2/。' },
  { at: ago(18), from: 'worker:建模/数据处理', text: '附件 3 有 214 行缺失，按相邻时刻线性插值，已记入 STATE.md。' },
  { at: ago(41), from: 'controller', text: '你选了路线 B。建模负责人开始问题三和问题四，论文负责人先搭提纲。' },
  { at: ago(95), text: '请所有 agent：图一律用 PDF 矢量输出，黑白打印可读。' },
  { at: ago(140), from: 'controller', text: '题目已拆成 4 个子问题，变量编号见 reports/ANALYSIS.md。' },
]

export function projectView(): ProjectView {
  const done = tasks.filter((t) => t.status === 'done').length
  const roles: { id: string; live?: RoleNode['live'] }[] = []
  const walk = (n: RoleNode) => {
    roles.push({ id: n.id, live: n.live })
    n.children.forEach(walk)
  }
  walk(org)
  return {
    root: ROOT,
    name: '数学建模竞赛',
    autopilot: true,
    progress: { done, total: tasks.length },
    roles,
    needs: 2,
    brief:
      '## 目标\n参加 2026 年全国大学生数学建模竞赛（国赛）B 题，**72 小时内**交出一篇不超过 25 页的论文。\n\n## 约定\n- 论文用中文，LaTeX 排版，图用 PDF 矢量。\n- 每个结果标明状态：代码已写、程序已运行、结果已核验。\n- 论文里不出现队员姓名与学校。\n\n## 分工\n建模负责人带数据处理和模型求解；论文负责人带正文写作和图表制作。',
    plan: '',
    milestones,
    tasks,
    org,
    team,
    notices,
  }
}

export const needs: NeedItem[] = [
  {
    id: 'n-gate',
    kind: 'gate',
    title: '中期评审：四问结果是否可以进入写作？',
    detail: '问题三的灵敏度分析补做后，R² 从 0.91 升到 0.96；请看 reports/Q3.md。',
    createdAt: ago(4),
    source: { kind: 'run', dir: RUN_DIR, step: 'review' },
    choices: [{ id: 'approve', label: '结果可信，进入写作' }],
    sendBackTargets: [{ step: 'solve', label: '逐问建模求解' }, { step: 'foundation', label: '假设与符号' }],
  },
  {
    id: 'n-q',
    kind: 'question',
    title: '图制作：图 3 的配色用灰度还是彩色？',
    detail: '赛题要求黑白打印，但彩色图在电子版更清楚。',
    createdAt: ago(9),
    source: { kind: 'project', root: ROOT, role: 'worker:论文/图表制作' },
    terminalId: 'term-w4',
  },
  {
    id: 'n-stall',
    kind: 'stalled',
    title: '论文负责人停住了',
    detail: '它在等问题三的结果，12 分钟没有新动作。',
    createdAt: ago(12),
    source: { kind: 'project', root: ROOT, role: 'lead:论文' },
    terminalId: 'term-l2',
  },
]

export const agents: LiveAgent[] = [
  { terminalId: 'term-ctl', workspaceId: 'ws1', agent: 'claude', model: 'opus-4.5', state: 'working', bound: { kind: 'role', root: ROOT, role: 'controller' }, since: ago(40) },
  { terminalId: 'term-l1', workspaceId: 'ws1', agent: 'claude', model: 'sonnet-4.5', state: 'working', bound: { kind: 'role', root: ROOT, role: 'lead:建模' }, since: ago(22) },
  { terminalId: 'term-w1', workspaceId: 'ws1', agent: 'gemini', state: 'working', bound: { kind: 'role', root: ROOT, role: 'worker:建模/数据处理' }, since: ago(15) },
  { terminalId: 'term-w2', workspaceId: 'ws1', agent: 'deepseek', state: 'working', bound: { kind: 'role', root: ROOT, role: 'worker:建模/模型求解' }, since: ago(33) },
  { terminalId: 'term-rb', workspaceId: 'ws1', agent: 'claude', state: 'working', bound: { kind: 'step', dir: RUN_DIR, step: 'robustness' }, since: ago(7) },
]

// ---------- the CUMCM run ----------

const rep = (s: string) => s.trim()
const ok = (label: string, detail?: string) => ({ label, ok: true as const, detail })

export function runView(): RunView {
  const steps: StepView[] = [
    {
      id: 'analysis', title: '读题与拆解', needs: [], kind: 'agent', status: 'passed',
      prompt: '精读题目和全部附件，拆出子问题，统一变量编号和单位，扫描数据，写成 reports/ANALYSIS.md。',
      attempts: [{ n: 1, startedAt: ago(130), endedAt: ago(104), agent: 'claude', outcome: 'passed', checks: [ok('子问题列表从 Q1 开始编号'), ok('reports/ANALYSIS.md 已生成')], report: rep(`
## 读题与拆解

题目要求为一条 **有 17 个站点的物流网络** 设计调度方案，共 4 个子问题：

| 子问题 | 目标 | 数据 |
| --- | --- | --- |
| Q1 | 估计各站点需求量 | 附件 1 |
| Q2 | 拟合运输时间 | 附件 2 |
| Q3 | 灵敏度与稳健性 | 附件 3 |
| Q4 | 总成本最小的调度 | 全部 |

- 有歧义的表述：「高峰期」未定义，按 7:00–9:00 与 17:00–19:00 处理
- 数据缺失：附件 3 有 214 行缺失
`) }],
    },
    {
      id: 'models', title: '建模路线', needs: ['analysis'], kind: 'gate', status: 'passed',
      attempts: [{ n: 1, startedAt: ago(104), endedAt: ago(88), agent: 'claude', outcome: 'passed', checks: [ok('route_options.json 含 2 条以上路线')], report: rep('三条候选路线：A 纯统计、B 机理模型加整数规划、C 强化学习。**推荐 B**：可解释，求解成本可控。') }],
    },
    {
      id: 'foundation', title: '假设与符号', needs: ['analysis'], kind: 'agent', status: 'passed',
      attempts: [{ n: 1, startedAt: ago(88), endedAt: ago(74), agent: 'claude', outcome: 'passed', checks: [ok('写了模型假设'), ok('写了符号表')], report: '共 9 条假设，每条标注依据与状态；符号表 41 项，已与 ANALYSIS.md 对齐。' }],
    },
    {
      id: 'solve', title: '逐问建模求解', needs: ['models', 'foundation'], kind: 'each', status: 'passed',
      items: [
        { id: 'Q1', label: '问题一', status: 'passed' },
        { id: 'Q2', label: '问题二', status: 'passed' },
        { id: 'Q3', label: '问题三', status: 'passed' },
        { id: 'Q4', label: '问题四', status: 'passed' },
      ],
      sendBack: ['foundation'],
      attempts: [
        { n: 1, startedAt: ago(74), endedAt: ago(40), agent: 'claude', outcome: 'sent_back', checks: [ok('四问都有报告'), { label: '问题三做了灵敏度分析', ok: false, detail: 'reports/Q3.md 没有出现「灵敏度」', log: 'contains reports/Q3.md /灵敏度|敏感性|[Ss]ensitivity/ → no match' }], report: '问题三仅给出点估计，缺少灵敏度分析。被中期评审退回。' },
        { n: 2, startedAt: ago(32), endedAt: ago(5), agent: 'claude', outcome: 'passed', checks: [ok('四问都有报告'), ok('问题三做了灵敏度分析'), ok('本问的结果已核验')], report: rep(`
## 逐问结果

- **Q1** 需求估计 MAPE = 4.1%，已核验
- **Q2** 运输时间拟合 R² = 0.962，已核验
- **Q3** 补做灵敏度分析：参数 θ 在 ±20% 内结论不变
- **Q4** 最优调度成本 18.74 万元，比基线低 11.3%

\`\`\`
python code/q4_solve.py --seed 7
objective = 187403.2   gap = 0.00%
\`\`\`
`) },
      ],
    },
    {
      id: 'review', title: '中期评审', needs: ['solve'], kind: 'gate', status: 'waiting', sendBack: ['solve', 'foundation'],
      prompt: '看四问的结果和报告，决定是进入写作，还是退回补做。',
      attempts: [
        { n: 1, startedAt: ago(40), endedAt: ago(37), outcome: 'sent_back', checks: [], report: '你退回到「逐问建模求解」：问题三要补灵敏度分析。' },
        { n: 2, startedAt: ago(4), checks: [], report: '问题三补做灵敏度后，所有结果都已核验。请确认是否进入写作。' },
      ],
    },
    {
      id: 'robustness', title: '稳健性与评价', needs: ['solve'], kind: 'agent', status: 'running', terminalId: 'term-rb',
      prompt: '对关键参数做扰动实验，给出误差范围，并从多个角度评价模型。',
      attempts: [{ n: 1, startedAt: ago(7), agent: 'claude', checks: [{ label: '报告包含稳健性实验', ok: null }, { label: '评价章节有优缺点对比', ok: null }], report: '正在对 θ、c 两个参数各取 9 个扰动点重算……' }],
    },
    { id: 'writing', title: '论文撰写', needs: ['review', 'robustness'], kind: 'agent', status: 'pending', attempts: [], prompt: '按全部报告写 LaTeX 论文，每个数都要能在 results/ 里找到出处。' },
    { id: 'verify', title: '逐项验收', needs: ['writing'], kind: 'agent', status: 'pending', sendBack: ['writing'], attempts: [], prompt: '对照竞赛规则逐项验收：页数、匿名、引用台账、AI 使用披露。' },
  ]
  return {
    dir: RUN_DIR, templateId: 'mathmodel', templateName: '数学建模竞赛', state: 'waiting', startedAt: ago(131),
    progress: { done: 4, total: steps.length }, currentStep: 'review',
    inputs: { 赛题: 'problem/B题.pdf、附件 1–3.xlsx', 竞赛: 'CUMCM', 选题: '已经选好', 论文语言: '中文', 论文状态: '草稿' },
    steps,
    decisions: [
      { at: ago(88), step: 'models', choice: '路线 B：机理模型加整数规划' },
      { at: ago(37), step: 'review', choice: '退回', sendBackTo: 'solve', note: '问题三缺少灵敏度分析，补做并核验。' },
    ],
    experience: ['附件里的单位常常写在表头括号里，读数据前先统一换算。', '整数规划建议先用线性松弛确认量级，再用求解器精确求解。', '逐问报告里必须标明结果状态，评审时才好核对。'],
  }
}

export function run2View(): RunView {
  const st = (id: string, title: string, status: StepView['status'], needs: string[]): StepView => ({ id, title, needs, kind: 'agent', status, attempts: [] })
  return {
    dir: RUN_DIR2, templateId: 'feature', templateName: '功能开发', state: 'running', startedAt: ago(52), progress: { done: 2, total: 5 }, currentStep: 'impl',
    inputs: { 需求: '支持分期支付回调的幂等处理' },
    steps: [st('spec', '需求澄清', 'passed', []), st('design', '方案设计', 'passed', ['spec']), st('impl', '实现', 'running', ['design']), st('test', '测试', 'pending', ['impl']), st('review', '代码评审', 'pending', ['test'])],
    decisions: [], experience: [],
  }
}

export const templates: TemplateSummary[] = [
  { id: 'mathmodel', name: '数学建模竞赛', description: '从赛题到论文：读题拆解、选路线、逐问求解、写论文、逐项验收。', source: 'builtin', steps: 9, inputs: [{ id: 'problem', label: '赛题', kind: 'files', required: true }, { id: 'contest', label: '竞赛', kind: 'choice', required: false, default: 'CUMCM' }, { id: 'notes', label: '你的想法', kind: 'long', required: false }] },
  { id: 'feature', name: '功能开发', description: '澄清需求、设计、实现、测试、评审。', source: 'user', steps: 5, inputs: [{ id: 'req', label: '需求', kind: 'long', required: true }] },
  { id: 'bugfix', name: '缺陷修复', description: '复现、定位、修复并补回归测试。', source: 'project', steps: 4, inputs: [{ id: 'issue', label: '问题描述', kind: 'long', required: true }] },
]

export const TEMPLATE_TOML = `id = "mathmodel"
name = "数学建模竞赛"
description = "从赛题到论文：读题拆解、选路线、逐问求解、写论文、逐项验收"
version = 2

# Shared context for every step.
preamble = """
你在参加数学建模竞赛（{{input.contest}}）。赛题在：{{input.problem}}。
结果分三种状态：代码已写、程序已运行、结果已核验。
"""

[[inputs]]
key = "problem"
label = "赛题"
kind = "files"
required = true

[[inputs]]
key = "contest"
label = "竞赛"
kind = "choice"
options = ["CUMCM", "MCM/ICM"]

[defaults]
effort = "high"
attempts = 2

[[steps]]
id = "analysis"
title = "读题与拆解"
summary = "拆出子问题，统一变量编号，摸清数据"
agent = "claude"
needs = []
prompt = """
精读题目和全部附件，拆出子问题并给关键变量统一编号。
写成 reports/ANALYSIS.md，并把子问题列表存到 reports/subproblems.json。
"""
outputs = ["reports/ANALYSIS.md", "reports/subproblems.json"]
checks = [
  { kind = "contains", path = "reports/subproblems.json", pattern = "\\"Q1\\"", message = "子问题列表从 Q1 开始编号" },
]

[[steps]]
id = "models"
title = "建模路线"
summary = "比较候选模型，给出几条完整路线让你选"
agent = "claude"
prompt = """
为每个子问题比较 2–4 个候选模型，写成 reports/MODEL_SELECTION.md，
再组合出 2–4 条完整路线。
"""
outputs = ["reports/MODEL_SELECTION.md", "reports/route_options.json"]
decide = { options = "reports/route_options.json", question = "采用哪条建模路线？" }

[[steps]]
id = "foundation"
title = "假设与符号"
summary = "每条假设都有依据，符号表全文统一"
needs = ["analysis"]
prompt = """
整理共用的假设、符号表和术语表。
"""
outputs = ["reports/FOUNDATION.md"]
checks = [
  { kind = "contains", path = "reports/FOUNDATION.md", pattern = "假设", message = "写了模型假设" },
]

[[steps]]
id = "solve"
title = "逐问建模求解"
summary = "每个子问题单独建模、求解、核验"
needs = ["models", "foundation"]
each = { from = "reports/subproblems.json", max = 8 }
attempts = 2
prompt = """
只做子问题 {{item.id}}：建立模型、写代码、运行并核验，写成 reports/{{item.id}}.md。
"""
outputs = ["reports/{{item.id}}.md"]
checks = [
  { kind = "contains", path = "reports/{{item.id}}.md", pattern = "已核验", message = "本问的结果已核验" },
  { kind = "contains", path = "reports/{{item.id}}.md", pattern = "灵敏度", message = "做了灵敏度分析" },
]

[[steps]]
id = "review"
title = "中期评审"
needs = ["solve"]
gate = true
fixes = ["solve", "foundation"]

[[steps]]
id = "robustness"
title = "稳健性与评价"
needs = ["solve"]
prompt = """
对关键参数做扰动实验，给出误差范围，并从多个角度评价模型。
"""
outputs = ["reports/ROBUSTNESS.md"]

[[steps]]
id = "writing"
title = "论文撰写"
needs = ["review", "robustness"]
prompt = """
按全部报告写 LaTeX 论文，每个数都要能在 results/ 里找到出处。
"""
outputs = ["paper/main.tex"]

[[steps]]
id = "verify"
title = "逐项验收"
needs = ["writing"]
fixes = ["writing"]
attempts = 2
checks = [
  { kind = "pdf_pages", path = "paper/main.pdf", max = 25 },
  { kind = "absent", path = "paper", pattern = "队员", message = "论文里没有身份信息" },
]
`

// A second project for the sidebar.
export const project2Summary = {
  root: ROOT2, name: '支付网关重构', autopilot: false, progress: { done: 3, total: 11 },
  roles: [{ id: 'controller', live: 'idle' as const }, { id: 'lead:后端', live: 'idle' as const }, { id: 'worker:后端/接口', live: 'done' as const }],
  needs: 0,
}
