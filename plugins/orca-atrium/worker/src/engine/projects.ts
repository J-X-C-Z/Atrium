// Projects: the board data, role agents (controller, leads, workers), the autopilot scheduler and
// stand-ins when an agent hits a usage limit. Project state lives in `.atrium/` through the core.

import { existsSync } from 'node:fs'
import { basename, join } from 'node:path'
import { normalizeActions, normalizePick, type Pick } from '../coreapi.js'
import { HostError } from '../host.js'
import type { AgentState, Member, Milestone, Notice, ProjectSummary, ProjectView, Quota, RoleNode, Task, TaskStatus, ValidationError } from '../protocol.js'
import { T, answerText, roleFooter, standInNote } from '../i18n.js'
import type { Binding, ProjectMeta } from '../state.js'
import { contains } from '../workspaces.js'
import { errorText, type Ctx } from './context.js'
import type { StandInSource } from './runs.js'

const STATUSES: TaskStatus[] = ['todo', 'ready', 'in_progress', 'review', 'blocked', 'done']
/** Default length of a usage limit when the agent's text gives no reset time. */
const LIMIT_MS = 5 * 3600_000

/** Idle this long (no turn, no wake): the autopilot nudges the role with a heartbeat. */
const HEARTBEAT_MS = 10 * 60_000

/** One agent to start, as `project.role.start` and `project.next` describe it. */
export interface LaunchSpec {
  role: string
  agent: string
  prompt: string
  cwd?: string
  worktree: boolean
  task?: string
  member?: string
  model?: string
  standingInFor?: string
}

export function launchSpec(raw: Record<string, any>, defaultAgent: string): LaunchSpec {
  return {
    role: String(raw.role ?? ''),
    agent: text(raw.agent) ?? defaultAgent,
    prompt: String(raw.prompt ?? ''),
    ...(text(raw.cwd) ? { cwd: raw.cwd } : {}),
    worktree: raw.worktree === true,
    ...(text(raw.task) ? { task: raw.task } : {}),
    ...(text(raw.member) ? { member: raw.member } : {}),
    ...(text(raw.model) ? { model: raw.model } : {}),
    ...(text(raw.standingInFor) ? { standingInFor: raw.standingInFor } : {})
  }
}

const obj = (v: unknown): Record<string, any> => (v && typeof v === 'object' ? (v as Record<string, any>) : {})
const text = (v: unknown): string | undefined => (typeof v === 'string' && v ? v : undefined)

export function mapTask(raw: unknown): Task {
  const r = obj(raw)
  const status = STATUSES.includes(r.status) ? (r.status as TaskStatus) : r.status === 'in-progress' || r.status === 'doing' ? 'in_progress' : 'todo'
  const task: Task = { id: String(r.id ?? ''), title: String(r.title ?? r.id ?? ''), status }
  if (text(r.owner)) task.owner = r.owner
  if (text(r.milestone)) task.milestone = r.milestone
  if (text(r.phase)) task.phase = r.phase
  if (text(r.file ?? r.path)) task.file = r.file ?? r.path
  if (r.pool === true) task.pool = true
  const at = r.updatedAt ?? r.updated_at
  if (typeof at === 'number') task.updatedAt = at
  return task
}

export function mapMember(raw: unknown): Member {
  const r = obj(raw)
  const m: Member = { name: String(r.name ?? ''), agent: String(r.agent ?? '') }
  if (text(r.model)) m.model = r.model
  if (text(r.effort)) m.effort = r.effort
  if (Array.isArray(r.folders) && r.folders.length) m.folders = r.folders.map(String)
  if (text(r.notes)) m.notes = r.notes
  const standIns = r.standIns ?? r.stand_ins
  if (Array.isArray(standIns) && standIns.length) m.standIns = standIns.map(String)
  return m
}

export function mapErrors(raw: unknown): ValidationError[] {
  if (!Array.isArray(raw)) return typeof raw === 'string' && raw ? [{ message: raw }] : []
  return raw.map((e) => (typeof e === 'string' ? { message: e } : { ...(typeof obj(e).line === 'number' ? { line: obj(e).line } : {}), message: String(obj(e).message ?? e) }))
}

function flatten(node: RoleNode, out: RoleNode[] = []): RoleNode[] {
  out.push(node)
  for (const c of node.children) flatten(c, out)
  return out
}

export function roleOf(id: string): { kind: 'controller' } | { kind: 'lead'; name: string } | { kind: 'worker'; lead: string; name: string } {
  if (id.startsWith('lead:')) return { kind: 'lead', name: id.slice(5) }
  if (id.startsWith('worker:')) {
    const [lead = '', name = ''] = id.slice(7).split('/')
    return { kind: 'worker', lead, name }
  }
  return { kind: 'controller' }
}

export class Projects implements StandInSource {
  private chains = new Map<string, Promise<void>>()
  private inflight = new Set<string>()
  private failed = new Set<string>()
  /** Autopilot wakes that are due, by project; `project.next` consumes them. */
  private wakes = new Map<string, { role: string; why: string }[]>()
  private lastWake = new Map<string, number>()

  constructor(private ctx: Ctx) {}

  // ---------- known projects ----------

  metas(): ProjectMeta[] {
    return this.ctx.store.data.projects
  }

  meta(root: string): ProjectMeta | undefined {
    return this.metas().find((p) => p.root === root)
  }

  register(root: string, workspaceId?: string): ProjectMeta {
    let meta = this.meta(root)
    if (!meta) {
      meta = { root }
      this.metas().push(meta)
    }
    if (workspaceId) meta.workspaceId = workspaceId
    this.ctx.store.save()
    return meta
  }

  /** Workspaces whose folder holds a project become known projects. */
  async discover(): Promise<void> {
    for (const ws of await this.ctx.workspaces.list()) {
      if ((ws.hostId ?? 'local') !== 'local') continue
      if (existsSync(join(ws.path, '.atrium', 'project.json'))) {
        const meta = this.meta(ws.path)
        if (!meta) this.register(ws.path, ws.id)
        else if (!meta.workspaceId) {
          meta.workspaceId = ws.id
          this.ctx.store.save()
        }
      }
    }
    // Forget projects whose folder is gone.
    const before = this.metas().length
    this.ctx.store.data.projects = this.metas().filter((p) => existsSync(join(p.root, '.atrium', 'project.json')))
    if (this.metas().length !== before) this.ctx.store.save()
  }

  private requireProject(root: string): ProjectMeta {
    const meta = this.meta(root)
    if (!meta) {
      if (existsSync(join(root, '.atrium', 'project.json'))) return this.register(root)
      throw new HostError('invalid_params', `${root} is not an Atrium project`)
    }
    return meta
  }

  // ---------- reading ----------

  private raw(root: string): Record<string, any> {
    return obj(this.ctx.core.call('project.open', { root }))
  }

  view(root: string): ProjectView {
    this.requireProject(root)
    const raw = this.raw(root)
    const tasks = (Array.isArray(raw.tasks) ? raw.tasks : []).map(mapTask)
    const done = tasks.filter((t) => t.status === 'done').length
    const org = this.decorate(raw.org && typeof raw.org === 'object' ? (raw.org as RoleNode) : { id: 'controller', role: { kind: 'controller' }, children: [] }, root, tasks)
    const roles = flatten(org).map((n) => ({ id: n.id, ...(n.live ? { live: n.live } : {}) }))
    const view: ProjectView = {
      root,
      name: String(raw.name ?? basename(root)),
      autopilot: raw.autopilot === true || obj(raw.autopilot).on === true,
      progress: raw.progress && typeof raw.progress === 'object' ? { done: Number(raw.progress.done ?? done), total: Number(raw.progress.total ?? tasks.length) } : { done, total: tasks.length },
      roles,
      needs: this.ctx.needs.forRoot(root),
      brief: String(raw.brief ?? ''),
      plan: String(raw.plan ?? ''),
      milestones: (Array.isArray(raw.milestones) ? raw.milestones : []) as Milestone[],
      tasks,
      org,
      team: (Array.isArray(raw.team) ? raw.team : Array.isArray(obj(raw.team).members) ? obj(raw.team).members : []).map(mapMember),
      notices: (Array.isArray(raw.notices) ? raw.notices : []).map((n: unknown): Notice => {
        const r = obj(n)
        return { at: Number(r.at ?? 0), ...(text(r.from) ? { from: r.from } : {}), text: String(r.text ?? '') }
      })
    }
    return view
  }

  summary(root: string): ProjectSummary | null {
    try {
      const { brief: _b, plan: _p, milestones: _m, tasks: _t, org: _o, team: _tm, notices: _n, ...summary } = this.view(root)
      return summary
    } catch (error) {
      this.ctx.host.log(`projects: cannot read ${root}: ${errorText(error)}`)
      return null
    }
  }

  /** Live state, terminal, current task and quota on every node. */
  private decorate(node: RoleNode, root: string, tasks: Task[]): RoleNode {
    const binding = this.ctx.store.forRole(root, node.id)
    const member = binding?.member ?? node.member
    const live: AgentState | undefined = binding ? (this.ctx.agents.stateOf(binding.terminalId) ?? 'idle') : undefined
    const quota = member ? this.ctx.store.data.quotas[member] : undefined
    const current = node.currentTask ?? binding?.task ?? tasks.find((t) => t.owner === node.id && t.status === 'in_progress')?.title
    const out: RoleNode = { ...node, children: (node.children ?? []).map((c) => this.decorate(c, root, tasks)) }
    if (member) out.member = member
    if (binding?.agent) out.agent = binding.agent
    if (live) out.live = live
    if (binding?.terminalId) out.terminalId = binding.terminalId
    if (current) out.currentTask = current
    if (quota) out.quota = limitedNow(quota, this.ctx.now()) ? quota : { ...quota, limited: false }
    if (binding?.standingInFor) out.standingInFor = binding.standingInFor
    return out
  }

  // ---------- requests ----------

  async init(params: { root?: string; brief?: string }): Promise<{ root: string }> {
    const ws = params.root ? await this.ctx.workspaces.forPath(params.root) : await this.ctx.workspaces.focusedWorkspace()
    const root = params.root ?? ws?.path
    if (!root) throw new HostError('invalid_params', T.noWorkspace(this.ctx.lang()))
    this.ctx.core.call('project.init', { root, ...(params.brief ? { brief: params.brief } : {}) })
    this.register(root, ws?.id)
    if (this.ctx.settings().autopilotDefault) this.ctx.core.call('project.autopilot.set', { root, on: true })
    this.changed(root)
    return { root }
  }

  setAutopilot(root: string, on: boolean): void {
    this.requireProject(root)
    this.ctx.core.call('project.autopilot.set', { root, on })
    this.changed(root)
    if (on) void this.pump(root)
  }

  taskCreate(p: { root: string; title: string; owner?: string; milestone?: string; pool?: boolean }): Task {
    this.requireProject(p.root)
    const { root, ...rest } = p
    const task = mapTask(this.ctx.core.call('project.task.create', { root, ...rest }))
    this.changed(root)
    void this.pump(root)
    return task
  }

  taskUpdate(p: { root: string; id: string; patch: Record<string, unknown> }): Task {
    this.requireProject(p.root)
    const task = mapTask(this.ctx.core.call('project.task.update', { root: p.root, id: p.id, patch: p.patch }))
    this.changed(p.root)
    return task
  }

  taskMove(p: { root: string; id: string; status: TaskStatus }): Task {
    this.requireProject(p.root)
    const task = mapTask(this.ctx.core.call('project.task.move', { root: p.root, id: p.id, status: p.status }))
    this.changed(p.root)
    void this.pump(p.root)
    return task
  }

  /** A notice; from an agent it carries the role's name (the core's notice op is the person's). */
  notice(root: string, textValue: string, from?: string): void {
    this.requireProject(root)
    this.ctx.core.call('project.notice.add', { root, text: from ? `[${from}] ${textValue}` : textValue })
    this.changed(root)
  }

  teamGet(root: string): { members: Member[]; errors: ValidationError[] } {
    this.requireProject(root)
    const raw = obj(this.ctx.core.call('team.read', { root }))
    return { members: (Array.isArray(raw.members) ? raw.members : []).map(mapMember), errors: mapErrors(raw.errors) }
  }

  teamSave(root: string, members: Member[]): { errors: ValidationError[] } {
    this.requireProject(root)
    const raw = obj(this.ctx.core.call('team.write', { root, members }))
    this.changed(root)
    return { errors: mapErrors(raw.errors) }
  }

  changed(root: string): void {
    this.ctx.host.post('project.changed', { root })
    this.ctx.host.post('overview.changed')
  }

  // ---------- roles ----------

  /** What the core says to launch for a role (`project.role.start`), spent members skipped. */
  private plan(root: string, role: string, agent?: string): LaunchSpec {
    const raw = obj(this.ctx.core.call('project.role.start', { root, role, ...(agent ? { agent } : {}), quotas: this.spent(this.ctx.now()), now: Math.floor(this.ctx.now() / 1000) }))
    const l = obj(raw.launch ?? raw)
    return launchSpec({ ...l, role }, this.ctx.settings().defaultAgent)
  }

  /** Starts a role's agent (the person pressed Start, or autopilot asked). */
  async startRole(root: string, role: string): Promise<Binding | null> {
    this.requireProject(root)
    const existing = this.ctx.store.forRole(root, role)
    if (existing) return existing
    return this.launchSpec(root, this.plan(root, role))
  }

  /** Launches an agent as the core described it, then tells the core which terminal runs the role. */
  private async launchSpec(root: string, spec: LaunchSpec): Promise<Binding> {
    const meta = this.requireProject(root)
    let workspaceId = meta.workspaceId
    if (!workspaceId) workspaceId = (await this.ctx.workspaces.forPath(root))?.id
    if (!workspaceId) throw new HostError('action_failed', `no Orca workspace holds ${root}`)
    let where = ''
    if (spec.worktree) {
      const base = await this.ctx.workspaces.byId(workspaceId)
      if (base) {
        workspaceId = await this.ctx.workspaces.createWorktree(base, `${basename(root)}-${spec.role.replace(/[:/]/g, '-')}`)
        where = ''
      }
    } else if (spec.cwd && spec.cwd !== root) {
      where = this.ctx.lang() === 'zh' ? `【Atrium】请在文件夹 ${spec.cwd} 中工作（先 cd 进去）。\n` : `[Atrium] Work in the folder ${spec.cwd} (cd there first).\n`
    }
    const note = spec.standingInFor ? standInNote(this.ctx.lang(), spec.member ?? spec.agent, spec.standingInFor) : ''
    const footer = roleFooter(this.ctx.lang(), { root, role: spec.role, ...(spec.task ? { task: spec.task } : {}), cli: this.ctx.cli })
    const binding = await this.ctx.agents.launch({
      workspaceId,
      agent: spec.agent,
      prompt: `${note}${where}${spec.prompt}\n${footer}`,
      binding: {
        kind: 'role',
        root,
        role: spec.role,
        ...(spec.task ? { task: spec.task } : {}),
        ...(spec.member ? { member: spec.member } : {}),
        ...(spec.standingInFor ? { standingInFor: spec.standingInFor } : {})
      },
      env: { ATRIUM_PROJECT: root, ATRIUM_ROLE: spec.role, ...(spec.task ? { ATRIUM_TASK: spec.task } : {}) }
    })
    try {
      if (binding.terminalId) this.ctx.core.call('project.bind', { root, role: spec.role, thread: binding.terminalId })
    } catch (error) {
      this.ctx.host.log(`projects: bind ${spec.role}: ${errorText(error)}`)
    }
    return binding
  }

  async stopRole(root: string, role: string): Promise<void> {
    const b = this.ctx.store.forRole(root, role)
    if (b) await this.ctx.agents.release(b, true)
    this.changed(root)
  }

  /** A role's turn ended: the scheduler may wake it on the next pass. */
  wake(binding: Binding, why: 'turn_ended' | 'heartbeat' | 'stalled'): void {
    if (!binding.root || !binding.role) return
    const list = this.wakes.get(binding.root) ?? []
    if (!list.some((w) => w.role === binding.role && w.why === why)) list.push({ role: binding.role, why })
    this.wakes.set(binding.root, list)
    this.lastWake.set(`${binding.root}\u0000${binding.role}`, this.ctx.now())
    void this.pump(binding.root)
  }

  // ---------- the autopilot ----------

  pump(root: string): Promise<void> {
    const prev = this.chains.get(root) ?? Promise.resolve()
    const next = prev.then(() => this.pass(root)).catch((error) => this.ctx.host.log(`projects: ${root}: ${errorText(error)}`))
    this.chains.set(root, next)
    return next
  }

  async tick(): Promise<void> {
    await Promise.all(this.metas().map((m) => this.pump(m.root)))
  }

  async settle(): Promise<void> {
    for (let i = 0; i < 50; i++) {
      await Promise.all([...this.chains.values()])
      await new Promise((r) => setTimeout(r, 5))
      if (this.inflight.size === 0) {
        await Promise.all([...this.chains.values()])
        return
      }
    }
  }

  /** What `project.next` is told: who runs, who is mid-turn, wakes that are due, who is spent. */
  private stateFor(root: string): Record<string, unknown> {
    const now = this.ctx.now()
    const live = this.ctx.store.live().filter((b) => b.kind === 'role' && b.root === root)
    const mid = (b: Binding) => ['working', 'starting'].includes(this.ctx.agents.stateOf(b.terminalId) ?? 'idle')
    const wakes = this.wakes.get(root) ?? []
    this.wakes.delete(root)
    // A live agent that has been idle a long while gets a heartbeat.
    for (const b of live) {
      const at = this.lastWake.get(`${root}\u0000${b.role}`) ?? b.startedAt
      if (!mid(b) && now - at > HEARTBEAT_MS && !wakes.some((w) => w.role === b.role)) {
        wakes.push({ role: b.role!, why: 'heartbeat' })
        this.lastWake.set(`${root}\u0000${b.role}`, now)
      }
    }
    return {
      live: live.map((b) => b.role),
      busy: live.filter(mid).map((b) => b.role),
      wakes,
      maxRunning: Math.max(1, this.ctx.settings().maxParallel),
      quotas: this.spent(now),
      now: Math.floor(now / 1000)
    }
  }

  /** Spent members as `team.pick` wants them: name → Unix seconds when usable again. */
  private spent(now: number): Record<string, number> {
    const out: Record<string, number> = {}
    for (const [name, q] of Object.entries(this.ctx.store.data.quotas)) {
      if (limitedNow(q, now)) out[name] = Math.ceil((q.resetsAt ?? now + LIMIT_MS) / 1000)
    }
    return out
  }

  private async pass(root: string): Promise<void> {
    if (!this.meta(root)) return
    let raw: Record<string, any>
    try {
      raw = this.raw(root)
    } catch (error) {
      this.ctx.host.log(`projects: cannot read ${root}: ${errorText(error)}`)
      return
    }
    if (!(raw.autopilot === true || obj(raw.autopilot).on === true)) return
    const actions = normalizeActions(this.ctx.core.call('project.next', { root, ...this.stateFor(root) }))
    const briefs = new Set<string>()
    for (const action of actions) {
      const k = `${root}\u0000${action.kind}\u0000${'role' in action ? action.role : ''}\u0000${'task' in action ? (action.task ?? '') : ''}`
      switch (action.kind) {
        case 'launch': {
          if (this.inflight.has(k) || this.failed.has(k) || this.ctx.store.forRole(root, action.role)) break
          if (this.ctx.agents.running() >= this.ctx.settings().maxParallel) break
          this.inflight.add(k)
          void this.launchSpec(root, launchSpec(action as unknown as Record<string, any>, this.ctx.settings().defaultAgent))
            .catch((error) => {
              this.failed.add(k)
              try {
                // The core already marked the task running: tell it the start failed.
                this.ctx.core.call('project.stream.failed', { root, role: action.role, reason: errorText(error) })
              } catch {}
              this.raiseError(root, action.role, errorText(error))
            })
            .finally(() => {
              this.inflight.delete(k)
              this.changed(root)
            })
          break
        }
        case 'send': {
          const b = this.ctx.store.forRole(root, action.role)
          if (b?.terminalId) await this.ctx.agents.send(b.terminalId, action.text)
          break
        }
        case 'stop':
          await this.stopRole(root, action.role)
          break
        case 'need': {
          const id = action.needKind === 'brief' && action.task ? `brief:${root}:${action.task}` : `project:${root}:${action.role ?? ''}:${action.title}`
          if (action.needKind === 'brief') briefs.add(id)
          this.ctx.needs.add({
            id,
            kind: action.needKind === 'brief' ? 'brief' : 'question',
            title: action.title,
            ...(action.detail ? { detail: action.detail } : {}),
            createdAt: this.ctx.now(),
            source: { kind: 'project', root, ...(action.role ? { role: action.role } : {}) }
          })
          break
        }
      }
    }
    // Briefs the person already approved elsewhere (the board) are no longer asked for.
    this.ctx.needs.removeWhere((n) => n.kind === 'brief' && n.source.kind === 'project' && n.source.root === root && !briefs.has(n.id))
    this.changed(root)
  }

  private raiseError(root: string, role: string, message: string): void {
    this.ctx.host.log(`projects: ${root} ${role}: ${message}`)
    this.ctx.needs.add({
      id: `error:${root}:${role}`,
      kind: 'error',
      title: T.error(this.ctx.lang(), `${basename(root)} · ${role}`),
      detail: message,
      createdAt: this.ctx.now(),
      source: { kind: 'project', root, role }
    })
  }

  // ---------- agents talking back ----------

  /** `atrium task done|blocked <id> [note]`. */
  taskReport(binding: Binding, id: string, outcome: 'done' | 'blocked', note: string): string {
    const root = binding.root!
    this.ctx.core.call('project.task.move', { root, id, status: outcome })
    if (note.trim()) this.notice(root, `${id} ${outcome}: ${note.trim()}`, binding.role)
    this.ctx.agents.reported(binding)
    if (outcome === 'blocked') {
      this.ctx.needs.add({
        id: `task-blocked:${root}:${id}`,
        kind: 'blocked',
        title: this.ctx.lang() === 'zh' ? `任务 ${id} 受阻` : `Task ${id} is blocked`,
        ...(note ? { detail: note } : {}),
        createdAt: this.ctx.now(),
        source: { kind: 'project', root, ...(binding.role ? { role: binding.role } : {}) },
        ...(binding.terminalId ? { terminalId: binding.terminalId } : {})
      })
    }
    this.changed(root)
    void this.pump(root)
    return this.ctx.lang() === 'zh' ? `任务 ${id} 已记为 ${outcome}。` : `Task ${id} marked ${outcome}.`
  }

  /** `atrium status` for a role: what is assigned to it. */
  statusText(binding: Binding): string {
    const root = binding.root!
    const v = this.view(root)
    const mine = v.tasks.filter((t) => t.owner === binding.role && t.status !== 'done')
    const head = `${v.name} · ${binding.role}`
    if (!mine.length) return `${head}\n${this.ctx.lang() === 'zh' ? '（没有分配给你的任务）' : '(no tasks assigned to you)'}`
    return [head, ...mine.map((t) => `${t.id}  [${t.status}]  ${t.title}`)].join('\n')
  }

  /** `atrium ask`: a question only the person can answer. */
  ask(binding: Binding, question: string): string {
    const where = binding.kind === 'role' ? `${basename(binding.root ?? '')} · ${binding.role}` : `${binding.step}${binding.item ? ` / ${binding.item}` : ''}`
    this.ctx.needs.add({
      id: `ask:${binding.terminalId ?? binding.launchId}:${this.ctx.now()}`,
      kind: 'question',
      title: T.question(this.ctx.lang(), where),
      detail: question,
      createdAt: this.ctx.now(),
      source: binding.kind === 'role' ? { kind: 'project', root: binding.root!, ...(binding.role ? { role: binding.role } : {}) } : { kind: 'run', dir: binding.dir!, ...(binding.step ? { step: binding.step } : {}) },
      ...(binding.terminalId ? { terminalId: binding.terminalId } : {})
    })
    return this.ctx.lang() === 'zh' ? '已转给用户。请结束本轮，等待回复。' : 'Passed to the person. End your turn and wait for the answer.'
  }

  /** A lead's brief waits for the person: any answer but "reject" approves it. */
  approve(need: { id: string; source: { kind: string; root?: string } }, answer?: string): void {
    const root = need.source.root
    const prefix = root ? `brief:${root}:` : ''
    if (!root || !need.id.startsWith(prefix)) return
    this.ctx.core.call('project.task.approve', { root, id: need.id.slice(prefix.length), approve: answer !== 'reject' })
    this.changed(root)
    void this.pump(root)
  }

  async answer(need: { terminalId?: string }, answer: string): Promise<void> {
    if (need.terminalId && answer) await this.ctx.agents.send(need.terminalId, answerText(this.ctx.lang(), answer))
  }

  onBlocked(binding: Binding): void {
    const root = binding.root!
    this.ctx.needs.add({
      id: `blocked:${binding.terminalId}`,
      kind: 'blocked',
      title: T.blocked(this.ctx.lang(), `${basename(root)} · ${binding.role}`),
      createdAt: this.ctx.now(),
      source: { kind: 'project', root, ...(binding.role ? { role: binding.role } : {}) },
      ...(binding.terminalId ? { terminalId: binding.terminalId } : {})
    })
  }

  onGone(binding: Binding): void {
    if (binding.root) {
      this.changed(binding.root)
      void this.pump(binding.root)
    }
  }

  /** A role agent hit its limit: the core places a stand-in, or the person is asked. */
  async onQuota(binding: Binding, evidence: string): Promise<void> {
    const root = binding.root!
    const role = binding.role!
    const member = binding.member ?? this.teamGet(root).members.find((m) => m.agent === binding.agent)?.name ?? binding.agent
    this.markLimited(member)
    let spec: LaunchSpec | null = null
    try {
      spec = this.plan(root, role)
    } catch (error) {
      this.ctx.host.log(`projects: no stand-in plan for ${role}: ${errorText(error)}`)
    }
    const standIn = spec && spec.standingInFor && spec.member !== member ? spec : null
    this.ctx.needs.add({
      id: `quota:${binding.terminalId}`,
      kind: 'quota',
      title: T.quota(this.ctx.lang(), member, standIn?.member),
      detail: evidence,
      createdAt: this.ctx.now(),
      source: { kind: 'project', root, role },
      ...(binding.terminalId ? { terminalId: binding.terminalId } : {})
    })
    if (!standIn) return
    await this.ctx.agents.release(binding, true)
    try {
      await this.launchSpec(root, { ...standIn, standingInFor: member, ...(binding.task && !standIn.task ? { task: binding.task } : {}) })
    } catch (error) {
      this.raiseError(root, role, errorText(error))
    }
    this.changed(root)
  }

  private markLimited(member: string): void {
    const now = this.ctx.now()
    const old = this.ctx.store.data.quotas[member]
    this.ctx.store.data.quotas[member] = { ...(old ?? {}), limited: true, resetsAt: old?.resetsAt && old.resetsAt > now ? old.resetsAt : now + LIMIT_MS }
    this.ctx.store.save()
  }

  /** `team.pick` for `wanted`, with the members spent now skipped. */
  private pickFor(root: string, wanted: string): Pick | null {
    try {
      const raw = this.ctx.core.call('team.pick', { root, wanted, quotas: this.spent(this.ctx.now()), now: Math.floor(this.ctx.now() / 1000) })
      return normalizePick(raw)
    } catch (error) {
      this.ctx.host.log(`projects: team.pick: ${errorText(error)}`)
      return null
    }
  }

  /** For runs: a stand-in backend from the team of the project that holds the run folder. */
  standInFor(dir: string, binding: Binding): { agent: string; member: string } | null {
    const meta = this.metas().find((p) => contains(p.root, dir))
    if (!meta) return null
    const members = this.teamGet(meta.root).members
    const limited = members.filter((m) => m.agent === binding.agent)
    for (const m of limited) this.markLimited(m.name)
    const wanted = binding.member ?? limited[0]?.name
    if (!wanted) return null
    const pick = this.pickFor(meta.root, wanted)
    if (!pick || pick.member === wanted) return null
    const agent = pick.agent ?? members.find((m) => m.name === pick.member)?.agent
    return agent && agent !== binding.agent ? { agent, member: pick.member } : null
  }

  quotaOf(member: string): Quota | undefined {
    return this.ctx.store.data.quotas[member]
  }
}

export function limitedNow(q: Quota, now: number): boolean {
  return q.limited === true && (q.resetsAt === undefined || q.resetsAt > now)
}
