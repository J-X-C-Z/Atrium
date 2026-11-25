// The worker's wiring: engine parts, the bridge socket, hooks, events, and a handler for every
// request in protocol.ts. Takes a Host and a core so tests can run it against fakes.

import { homedir } from 'node:os'
import { join } from 'node:path'
import { BridgeServer } from './bridge/server.js'
import { LaunchHook } from './bridge/hook.js'
import type { CoreApi } from './coreapi.js'
import { Agents } from './engine/agents.js'
import { makeCliHandler } from './engine/cli.js'
import { errorText, type Ctx } from './engine/context.js'
import { Needs } from './engine/needs.js'
import { Projects, mapErrors } from './engine/projects.js'
import { Runs } from './engine/runs.js'
import { HostError, type Disposable, type Host } from './host.js'
import { T, resolveLang } from './i18n.js'
import { openLink, readLink, writeBoardLink, writeRunLink } from './links.js'
import type { Lang, RequestName, Requests, TemplateSummary } from './protocol.js'
import { DEFAULTS, readSettings, type Settings } from './settings.js'
import { Store } from './state.js'
import { Workspaces } from './workspaces.js'

export interface ServiceOptions {
  core: CoreApi
  /** Folder with the `atrium` CLI, put on agents' PATH. */
  binDir: string
  /** Unix socket path or Windows pipe name. */
  socket: string
  /** The CLI as a full command, for agents without it on PATH. */
  cli: string
  /** Where the user's workflow templates live. */
  userTemplates?: string
  /** After a turn ends with no report: wait this long, nudge, wait again, flag. */
  graceMs?: number
  tickMs?: number
  now?: () => number
}

type Handlers = { [K in RequestName]: (params: Requests[K]['params']) => Promise<Requests[K]['result']> | Requests[K]['result'] }

export class Service {
  readonly store!: Store
  readonly ctx!: Ctx
  readonly runs!: Runs
  readonly projects!: Projects
  readonly needs!: Needs
  readonly agents!: Agents
  readonly bridge!: BridgeServer
  readonly workspaces!: Workspaces
  private current: Settings = { ...DEFAULTS }
  private disposables: Disposable[] = []
  private timer: NodeJS.Timeout | null = null

  private constructor(
    readonly host: Host,
    readonly opts: ServiceOptions
  ) {}

  static async create(host: Host, opts: ServiceOptions): Promise<Service> {
    const s = new Service(host, opts)
    await s.init()
    return s
  }

  private async init(): Promise<void> {
    const { host, opts } = this
    this.current = await readSettings(host)
    const store = await Store.load(host)
    const workspaces = new Workspaces(host)
    const lang = (): Lang => resolveLang(this.current.language)
    const hook = new LaunchHook(opts.binDir)
    const agents = new Agents(host, store, hook, { graceMs: opts.graceMs ?? 45_000, socket: opts.socket, token: store.data.token }, lang)
    const needs = new Needs(host, store, lang)
    const ctx: Ctx = {
      host,
      core: opts.core,
      store,
      agents,
      needs,
      workspaces,
      settings: () => this.current,
      lang,
      cli: opts.cli,
      templateDirs: () => this.templateDirs(),
      now: opts.now ?? (() => Date.now())
    }
    const runs = new Runs(ctx)
    const projects = new Projects(ctx)
    runs.standIns = projects
    needs.countRunning(() => agents.running())
    Object.assign(this, { store, ctx, runs, projects, needs, agents, workspaces })
    Object.assign(this, { bridge: new BridgeServer(opts.socket, store.data.token, makeCliHandler(ctx, runs, projects), (m) => host.log(m)) })
    this.wireNeeds()
    this.wireAgents()
    await this.bridge.start()
    this.disposables.push(hook.register(host))
    this.wireEvents()
    this.wireRequests()
    this.wireCommands()
    this.disposables.push(host.onSettings(() => void readSettings(host).then((s) => ((this.current = s), void needs.updateStatusBar()))))
    await agents.reconcile()
    await needs.updateStatusBar()
    void this.tick()
    this.timer = setInterval(() => void this.tick(), opts.tickMs ?? 15_000)
    this.timer.unref?.()
  }

  /** Resumes anything that waits: runs, project autopilots. */
  async tick(): Promise<void> {
    try {
      await this.projects.discover()
      await Promise.all([this.runs.tick(), this.projects.tick()])
      await this.needs.updateStatusBar()
    } catch (error) {
      this.host.log(`tick: ${errorText(error)}`)
    }
  }

  /** Waits until everything started has settled (tests). */
  async settle(): Promise<void> {
    await this.runs.settle()
    await this.projects.settle()
    await this.runs.settle()
  }

  async dispose(): Promise<void> {
    if (this.timer) clearInterval(this.timer)
    for (const d of this.disposables.splice(0)) {
      try {
        d.dispose()
      } catch {}
    }
    this.agents.dispose()
    await this.bridge.stop()
    await this.store.flush()
  }

  private templateDirs(): string[] {
    const dirs = [this.opts.userTemplates ?? join(homedir(), '.config', 'Atrium', 'workflows')]
    for (const p of this.store.data.projects) dirs.push(join(p.root, '.atrium', 'workflows'))
    const focused = this.workspaces.focused?.workspaceId
    const ws = focused ? this.workspaces.cached(focused) : undefined
    if (ws) dirs.push(join(ws.path, '.atrium', 'workflows'))
    return [...new Set(dirs)]
  }

  // ---------- wiring ----------

  private wireNeeds(): void {
    const { needs, runs, projects } = this
    needs.onResolve('gate', async (need, answer) => {
      if (need.source.kind !== 'run' || !need.source.step) return
      await runs.decide({ dir: need.source.dir, step: need.source.step, choice: answer || need.choices?.[0]?.id || 'approve' })
    })
    needs.onResolve('brief', (need, answer) => projects.approve(need, answer))
    needs.onResolve('question', (need, answer) => projects.answer(need, answer ?? ''))
  }

  private wireAgents(): void {
    const { agents, runs, projects, needs, host } = this
    agents.events = {
      changed: (binding, state, terminalId) => {
        if (state === 'working') needs.removeWhere((n) => n.terminalId === terminalId && (n.kind === 'blocked' || n.kind === 'stalled'))
        if (binding?.dir) host.post('run.changed', { dir: binding.dir })
        if (binding?.root) host.post('project.changed', { root: binding.root })
        void needs.updateStatusBar()
      },
      stalled: (b) => (b.kind === 'role' ? undefined : runs.onStalled(b)),
      blocked: (b) => (b.kind === 'role' ? projects.onBlocked(b) : runs.onBlocked(b)),
      idle: (b) => {
        if (b.kind === 'role') projects.wake(b, 'turn_ended')
      },
      quota: (b, evidence) => void (b.kind === 'role' ? projects.onQuota(b, evidence) : runs.onQuota(b, evidence)),
      gone: (b) => (b.kind === 'role' ? projects.onGone(b) : runs.onGone(b))
    }
  }

  private wireEvents(): void {
    const { host, agents, workspaces } = this
    this.disposables.push(
      host.on('agent.status.changed', (p) => agents.onStatus(p)),
      host.on('workspace.changed', (p) => {
        workspaces.focused = p?.context ?? null
      }),
      host.on('workspace.created', () => workspaces.invalidate()),
      host.on('workspace.removed', () => workspaces.invalidate()),
      host.hook('workspace.create.after', () => {
        workspaces.invalidate()
        return undefined
      })
    )
  }

  private wireCommands(): void {
    const { host, workspaces, projects } = this
    const notify = (body: string) => host.call('notifications.show', { title: 'Atrium', body }).catch(() => {})
    const lang = () => this.ctx.lang()
    this.disposables.push(
      host.command('atrium.open', () => notify(T.openHint(lang()))),
      host.command('atrium.startWorkflow', () => notify(T.pickTemplate(lang()))),
      host.command('atrium.newProject', async () => {
        const { root } = await projects.init({})
        await this.openBoard(root)
      }),
      host.command('atrium.openBoard', async () => {
        const ws = await workspaces.focusedWorkspace()
        if (!ws) return notify(T.noWorkspace(lang()))
        if (!projects.meta(ws.path)) await projects.discover()
        if (!projects.meta(ws.path)) await projects.init({ root: ws.path })
        await this.openBoard(ws.path)
      })
    )
  }

  async openBoard(root: string): Promise<void> {
    const meta = this.projects.meta(root)
    await openLink(this.host, this.workspaces, writeBoardLink(root), meta?.workspaceId)
  }

  async openRun(dir: string): Promise<void> {
    const meta = this.runs.meta(dir)
    if (!meta) throw new HostError('invalid_params', `unknown run ${dir}`)
    await openLink(this.host, this.workspaces, writeRunLink(dir), meta.workspaceId)
  }

  private templates(): TemplateSummary[] {
    const raw: any = this.ctx.core.call('workflow.templates', { dirs: this.templateDirs() })
    const list: any[] = Array.isArray(raw) ? raw : Array.isArray(raw?.templates) ? raw.templates : []
    return list.map((t) => ({
      id: String(t.id ?? t.name ?? ''),
      name: String(t.name ?? t.id ?? ''),
      ...(t.description ? { description: String(t.description) } : {}),
      source: t.source === 'user' || t.source === 'project' ? t.source : 'builtin',
      ...(t.path ? { path: String(t.path) } : {}),
      steps: Array.isArray(t.steps) ? t.steps.length : Number(t.steps ?? 0),
      inputs: (Array.isArray(t.inputs) ? t.inputs : []).map((i: any) => ({
        id: String(i.id ?? i.name ?? ''),
        label: String(i.label ?? i.id ?? i.name ?? ''),
        kind: String(i.kind ?? i.type ?? 'text'),
        required: i.required === true,
        ...(i.default !== undefined && i.default !== null ? { default: String(i.default) } : {}),
        ...(Array.isArray(i.options) && i.options.length ? { options: i.options.map(String) } : {})
      })),
      ...(t.error ? { error: String(t.error) } : {})
    }))
  }

  private wireRequests(): void {
    const { projects, runs, needs, agents, host, ctx } = this
    const handlers: Handlers = {
      'overview.get': async () => {
        await projects.discover()
        return {
          projects: projects.metas().map((m) => projects.summary(m.root)).filter((p) => p !== null),
          runs: runs.metas().map((m) => runs.summary(m.dir)).filter((r) => r !== null),
          needs: needs.list(),
          agents: agents.list(),
          lang: ctx.lang()
        }
      },
      'project.get': ({ root }) => projects.view(root),
      'project.init': (p) => projects.init(p),
      'project.autopilot': ({ root, on }) => projects.setAutopilot(root, on),
      'project.task.create': (p) => projects.taskCreate(p),
      'project.task.update': (p) => projects.taskUpdate(p),
      'project.task.move': (p) => projects.taskMove(p),
      'project.notice': ({ root, text }) => projects.notice(root, text),
      'team.get': ({ root }) => projects.teamGet(root),
      'team.save': ({ root, members }) => projects.teamSave(root, members),
      'role.start': async ({ root, role }) => {
        await projects.startRole(root, role)
        projects.changed(root)
      },
      'role.stop': ({ root, role }) => projects.stopRole(root, role),
      'agent.focus': ({ terminalId }) => agents.focus(terminalId),
      'agent.nudge': ({ terminalId }) => agents.nudge(terminalId),
      'templates.list': () => this.templates(),
      'template.validate': ({ text }) => {
        const raw: any = ctx.core.call('workflow.template.validate', { text })
        const errors = mapErrors(Array.isArray(raw) ? raw : raw?.errors)
        return { errors, ...(Array.isArray(raw?.steps) ? { steps: raw.steps } : {}) }
      },
      'run.start': (p) => runs.start(p),
      'run.get': ({ dir }) => runs.view(dir),
      'run.decide': (p) => runs.decide(p),
      'run.pause': ({ dir }) => runs.control(dir, 'pause'),
      'run.resume': ({ dir }) => runs.control(dir, 'resume'),
      'run.cancel': ({ dir }) => runs.control(dir, 'cancel'),
      'run.retry': (p) => runs.retry(p),
      'need.resolve': ({ id, answer }) => needs.resolve(id, answer),
      'board.open': ({ root }) => this.openBoard(root),
      'run.open': ({ dir }) => this.openRun(dir),
      'view.expand': async ({ mode }) => {
        if (mode === 'sidebar') {
          const p = ctx.store.data.projects[0]
          if (p) {
            try {
              await this.openBoard(p.root)
              return
            } catch {}
          }
          const r = ctx.store.data.runs[0]
          if (r) {
            try {
              await this.openRun(r.dir)
              return
            } catch {}
          }
        }
        for (const action of ['sidebar.left.toggle', 'sidebar.right.toggle'] as const) {
          try {
            await ctx.host.call('commands.run', { action })
          } catch {}
        }
      },
      'link.read': ({ text }) => readLink(text)
    }
    for (const name of Object.keys(handlers) as RequestName[]) {
      const handler = handlers[name] as (p: any) => unknown
      this.disposables.push(
        host.handle(name, async (input) => {
          try {
            return (await handler(input ?? {})) ?? null
          } catch (error) {
            host.log(`${name}: ${errorText(error)}`)
            throw error
          }
        })
      )
    }
  }
}

