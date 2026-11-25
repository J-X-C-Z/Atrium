// The one contract between the Atrium worker and its panels/editors.
// UI → worker: `plugin.request` with { name, params } (see Orca's plugin-host-api.md).
// Worker → UI: `orca.panels.post(name, payload)`.
// Change this file only together with both sides.

export type Lang = 'zh' | 'en'

// ---------- shared shapes ----------

/** Live state of an agent as Orca reports it, plus ours. */
export type AgentState = 'working' | 'waiting' | 'blocked' | 'done' | 'idle' | 'stalled' | 'starting'

export interface LiveAgent {
  terminalId: string
  workspaceId: string | null
  /** Orca's agent kind, e.g. "claude", "codex", "gemini". */
  agent: string
  model?: string
  title?: string
  state: AgentState
  /** What this agent is doing for us, if anything. */
  bound?:
    | { kind: 'role'; root: string; role: string; task?: string }
    | { kind: 'step'; dir: string; step: string; item?: string }
  since: number
}

export type NeedKind = 'gate' | 'question' | 'stalled' | 'blocked' | 'brief' | 'quota' | 'error'

export interface NeedItem {
  id: string
  kind: NeedKind
  title: string
  detail?: string
  createdAt: number
  /** Where it came from, to open the right view. */
  source: { kind: 'project'; root: string; role?: string } | { kind: 'run'; dir: string; step?: string }
  terminalId?: string
  /** For gates: the choices and the steps it can send work back to. */
  choices?: { id: string; label: string }[]
  sendBackTargets?: { step: string; label: string }[]
}

// ---------- projects ----------

export type Role = { kind: 'controller' } | { kind: 'lead'; name: string } | { kind: 'worker'; lead: string; name: string }

export type TaskStatus = 'todo' | 'ready' | 'in_progress' | 'review' | 'blocked' | 'done'

export interface Task {
  id: string
  title: string
  status: TaskStatus
  /** Role id (see RoleNode.id) of the owner, if assigned. */
  owner?: string
  milestone?: string
  phase?: string
  /** Relative path of the task file, e.g. ".atrium/tasks/lead-api/t3.md". */
  file?: string
  pool?: boolean
  updatedAt?: number
}

export interface Milestone {
  id: string
  title: string
  phases: { id: string; title: string; done: number; total: number }[]
}

export interface Member {
  name: string
  agent: string
  model?: string
  effort?: string
  folders?: string[]
  notes?: string
  /** Ordered stand-ins (member names). */
  standIns?: string[]
}

export interface Quota {
  /** 0..1 used, when known. */
  used?: number
  resetsAt?: number
  limited?: boolean
}

export interface RoleNode {
  /** Stable id: "controller", "lead:<name>", "worker:<lead>/<name>". */
  id: string
  role: Role
  member?: string
  agent?: string
  model?: string
  live?: AgentState
  terminalId?: string
  currentTask?: string
  quota?: Quota
  /** Set when a stand-in is covering for the listed member. */
  standingInFor?: string
  children: RoleNode[]
}

export interface Notice {
  at: number
  from?: string
  text: string
}

export interface ProjectSummary {
  root: string
  name: string
  autopilot: boolean
  progress: { done: number; total: number }
  roles: { id: string; live?: AgentState }[]
  needs: number
}

export interface ProjectView extends ProjectSummary {
  brief: string
  plan: string
  milestones: Milestone[]
  tasks: Task[]
  org: RoleNode
  team: Member[]
  notices: Notice[]
}

// ---------- workflows ----------

export type RunState = 'running' | 'waiting' | 'paused' | 'done' | 'failed' | 'cancelled'
export type StepStatus = 'pending' | 'running' | 'checking' | 'waiting' | 'passed' | 'failed' | 'skipped' | 'sent_back'

export interface TemplateSummary {
  id: string
  name: string
  description?: string
  source: 'builtin' | 'user' | 'project'
  path?: string
  steps: number
  inputs: { id: string; label: string; kind: string; required: boolean; default?: string; options?: string[] }[]
  error?: string
}

export interface CheckView {
  label: string
  ok: boolean | null
  detail?: string
  log?: string
}

export interface AttemptView {
  n: number
  startedAt: number
  endedAt?: number
  agent?: string
  report?: string
  checks: CheckView[]
  outcome?: 'passed' | 'failed' | 'sent_back' | 'stalled'
}

export interface StepView {
  id: string
  title: string
  needs: string[]
  kind: 'agent' | 'gate' | 'each'
  status: StepStatus
  /** For `each` steps. */
  items?: { id: string; label: string; status: StepStatus }[]
  attempts: AttemptView[]
  prompt?: string
  /** Steps this one may send work back to (gates / reviews). */
  sendBack?: string[]
  /** Gate choices when waiting for a decision. */
  choices?: { id: string; label: string }[]
  terminalId?: string
}

export interface Decision {
  at: number
  step: string
  choice: string
  sendBackTo?: string
  note?: string
}

export interface RunSummary {
  dir: string
  templateId: string
  templateName: string
  state: RunState
  startedAt: number
  progress: { done: number; total: number }
  currentStep?: string
}

export interface RunView extends RunSummary {
  inputs: Record<string, string>
  steps: StepView[]
  decisions: Decision[]
  experience: string[]
}

export interface ValidationError {
  line?: number
  message: string
}

// ---------- requests ----------

export interface Requests {
  'overview.get': { params: void; result: { projects: ProjectSummary[]; runs: RunSummary[]; needs: NeedItem[]; agents: LiveAgent[]; lang: Lang } }
  'project.get': { params: { root: string }; result: ProjectView }
  'project.init': { params: { root?: string; brief?: string }; result: { root: string } }
  'project.autopilot': { params: { root: string; on: boolean }; result: void }
  'project.task.create': { params: { root: string; title: string; owner?: string; milestone?: string; pool?: boolean }; result: Task }
  'project.task.update': { params: { root: string; id: string; patch: Partial<Pick<Task, 'title' | 'owner' | 'milestone' | 'pool'>> }; result: Task }
  'project.task.move': { params: { root: string; id: string; status: TaskStatus }; result: Task }
  'project.notice': { params: { root: string; text: string }; result: void }
  'team.get': { params: { root: string }; result: { members: Member[]; errors: ValidationError[] } }
  'team.save': { params: { root: string; members: Member[] }; result: { errors: ValidationError[] } }
  'role.start': { params: { root: string; role: string }; result: void }
  'role.stop': { params: { root: string; role: string }; result: void }
  'agent.focus': { params: { terminalId: string }; result: void }
  'agent.nudge': { params: { terminalId: string }; result: void }
  'templates.list': { params: void; result: TemplateSummary[] }
  'template.validate': { params: { text: string }; result: { errors: ValidationError[]; steps?: StepView[] } }
  'run.start': { params: { templateId: string; inputs: Record<string, string>; workspaceId?: string }; result: { dir: string } }
  'run.get': { params: { dir: string }; result: RunView }
  'run.decide': { params: { dir: string; step: string; choice: string; sendBackTo?: string; note?: string }; result: void }
  'run.pause': { params: { dir: string }; result: void }
  'run.resume': { params: { dir: string }; result: void }
  'run.cancel': { params: { dir: string }; result: void }
  'run.retry': { params: { dir: string; step: string; item?: string }; result: void }
  'need.resolve': { params: { id: string; answer?: string }; result: void }
  'board.open': { params: { root: string }; result: void }
  'run.open': { params: { dir: string }; result: void }
  'link.read': { params: { text: string }; result: { kind: 'board'; root: string } | { kind: 'run'; dir: string } }
}

export type RequestName = keyof Requests

// ---------- posts ----------

export interface Posts {
  'overview.changed': void
  'project.changed': { root: string }
  'run.changed': { dir: string }
  'needs.changed': { count: number }
  'agent.changed': { terminalId: string; state: AgentState }
}

export type PostName = keyof Posts
