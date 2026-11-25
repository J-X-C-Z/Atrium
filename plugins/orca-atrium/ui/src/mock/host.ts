// A mock `window.orca` with a stateful fake worker, for dev.html and screenshots.
import type { Member, NeedItem, ProjectView, RoleNode, RunView, Task, ValidationError } from '../../../worker/src/protocol'
import type { OrcaFrame } from '../orca'
import { lintTemplate, readTemplate, TomlError } from '../template/toml'
import { t } from '../i18n'
import * as fx from './fixtures'

type Handler = (p: any) => unknown
const delay = <T>(v: T, ms = 60) => new Promise<T>((r) => setTimeout(() => r(structuredClone(v)), ms))

export interface MockOptions {
  theme: 'light' | 'dark'
  lang: 'zh' | 'en'
  doc?: string
  /** Simulate: 'empty' (no projects/runs/needs), 'error' (worker down), 'slow'. */
  scenario?: string
}

const LIGHT: Record<string, string> = {
  '--orca-background': '#ffffff', '--orca-foreground': '#171717', '--orca-card': '#ffffff', '--orca-card-foreground': '#171717',
  '--orca-popover': '#ffffff', '--orca-popover-foreground': '#171717', '--orca-primary': '#262626', '--orca-primary-foreground': '#fafafa',
  '--orca-secondary': '#f4f4f5', '--orca-secondary-foreground': '#262626', '--orca-muted': '#f4f4f5', '--orca-muted-foreground': '#737373',
  '--orca-accent': '#f1f1f2', '--orca-accent-foreground': '#262626', '--orca-destructive': '#dc2626', '--orca-destructive-foreground': '#fafafa',
  '--orca-border': '#e5e5e5', '--orca-input': '#e5e5e5', '--orca-ring': '#737373', '--orca-radius': '8px',
}
const DARK: Record<string, string> = {
  '--orca-background': '#181818', '--orca-foreground': '#ededed', '--orca-card': '#1e1e1e', '--orca-card-foreground': '#ededed',
  '--orca-popover': '#222222', '--orca-popover-foreground': '#ededed', '--orca-primary': '#e5e5e5', '--orca-primary-foreground': '#1a1a1a',
  '--orca-secondary': '#2a2a2a', '--orca-secondary-foreground': '#ededed', '--orca-muted': '#262626', '--orca-muted-foreground': '#a1a1a1',
  '--orca-accent': '#2c2c2c', '--orca-accent-foreground': '#ededed', '--orca-destructive': '#ef5350', '--orca-destructive-foreground': '#fafafa',
  '--orca-border': '#333333', '--orca-input': '#3a3a3a', '--orca-ring': '#8a8a8a', '--orca-radius': '8px',
}

export function installMock(opts: MockOptions) {
  const vars = opts.theme === 'dark' ? DARK : LIGHT
  const root = document.documentElement
  for (const k in vars) root.style.setProperty(k, vars[k])
  root.classList.add(opts.theme)
  root.style.setProperty('--orca-font-sans', `system-ui, -apple-system, 'Segoe UI', 'PingFang SC', 'Noto Sans CJK SC', 'Noto Sans SC', 'Microsoft YaHei', sans-serif`)

  const subs = new Map<string, Set<(p: any) => void>>()
  const post = (name: string, payload?: unknown) => setTimeout(() => subs.get(name)?.forEach((cb) => cb(payload)), 20)

  // ---- state ----
  const empty = opts.scenario === 'empty'
  const project: ProjectView = fx.projectView()
  let needs: NeedItem[] = empty ? [] : structuredClone(fx.needs)
  let run: RunView = fx.runView()
  const run2 = fx.run2View()
  let team: Member[] = structuredClone(fx.team)

  const recount = () => {
    project.progress = { done: project.tasks.filter((x) => x.status === 'done').length, total: project.tasks.length }
    project.needs = needs.filter((n) => n.source.kind === 'project').length
  }
  const findRole = (id: string, n: RoleNode = project.org): RoleNode | undefined => (n.id === id ? n : n.children.map((c) => findRole(id, c)).find(Boolean))
  const syncRoles = () => {
    project.roles = []
    const walk = (n: RoleNode) => {
      project.roles.push({ id: n.id, live: n.live })
      n.children.forEach(walk)
    }
    walk(project.org)
  }

  const handlers: Record<string, Handler> = {
    'overview.get': () => {
      if (opts.scenario === 'error') throw Object.assign(new Error('worker_unavailable'), { code: 'unavailable' })
      const live = fx.agents.filter((a) => a.state === 'working')
      recount()
      const { roles, needs: pn, progress, name, root: r, autopilot } = project
      void live
      return {
        projects: empty ? [] : [{ root: r, name, autopilot, progress, roles, needs: pn }, fx.project2Summary],
        runs: empty ? [] : [{ dir: run.dir, templateId: run.templateId, templateName: run.templateName, state: run.state, startedAt: run.startedAt, progress: run.progress, currentStep: run.currentStep }, { dir: run2.dir, templateId: run2.templateId, templateName: run2.templateName, state: run2.state, startedAt: run2.startedAt, progress: run2.progress, currentStep: run2.currentStep }],
        needs,
        agents: empty ? [] : fx.agents,
        lang: opts.lang,
      }
    },
    'project.get': () => {
      recount()
      syncRoles()
      return project
    },
    'project.init': () => ({ root: fx.ROOT }),
    'project.autopilot': (p) => {
      project.autopilot = p.on
      post('project.changed', { root: project.root })
      post('overview.changed')
    },
    'project.task.create': (p) => {
      const task: Task = { id: `t${project.tasks.length + 1}`, title: p.title, status: 'todo', owner: p.owner, milestone: p.milestone, pool: p.pool, file: `.atrium/tasks/t${project.tasks.length + 1}.md`, updatedAt: Date.now() }
      project.tasks.push(task)
      post('project.changed', { root: project.root })
      return task
    },
    'project.task.update': (p) => {
      const tk = project.tasks.find((x) => x.id === p.id)!
      Object.assign(tk, p.patch)
      post('project.changed', { root: project.root })
      return tk
    },
    'project.task.move': (p) => {
      const tk = project.tasks.find((x) => x.id === p.id)!
      tk.status = p.status
      tk.updatedAt = Date.now()
      recount()
      post('project.changed', { root: project.root })
      post('overview.changed')
      return tk
    },
    'project.notice': (p) => {
      project.notices.unshift({ at: Date.now(), text: p.text })
      post('project.changed', { root: project.root })
    },
    'team.get': () => ({ members: team, errors: [] as ValidationError[] }),
    'team.save': (p) => {
      team = p.members
      post('project.changed', { root: project.root })
      return { errors: [] }
    },
    'role.start': (p) => {
      const n = findRole(p.role)
      if (n) n.live = 'starting'
      setTimeout(() => {
        if (n) n.live = 'working'
        post('project.changed', { root: project.root })
      }, 900)
      post('project.changed', { root: project.root })
    },
    'role.stop': (p) => {
      const n = findRole(p.role)
      if (n) n.live = 'idle'
      post('project.changed', { root: project.root })
    },
    'agent.focus': () => undefined,
    'agent.nudge': () => undefined,
    'templates.list': () => fx.templates,
    'template.validate': (p) => {
      const errors: ValidationError[] = []
      try {
        const { template } = readTemplate(p.text)
        for (const pr of lintTemplate(template, { dup: (id) => t('tplDupId', { id }), unknown: (step, need) => t('tplUnknownNeed', { step, need }), cycle: (steps) => t('tplCycle', { steps }) })) errors.push({ line: pr.line, message: pr.message })
      } catch (e) {
        errors.push({ line: e instanceof TomlError ? e.line : undefined, message: (e as Error).message })
      }
      return { errors }
    },
    'run.start': () => ({ dir: fx.RUN_DIR }),
    'run.get': (p) => (p.dir === fx.RUN_DIR2 ? run2 : run),
    'run.decide': (p) => {
      const step = run.steps.find((s) => s.id === p.step)!
      const when = Date.now()
      if (p.sendBackTo) {
        step.status = 'pending'
        const tgt = run.steps.find((s) => s.id === p.sendBackTo)!
        tgt.status = 'running'
        run.currentStep = tgt.id
        run.state = 'running'
        run.decisions.push({ at: when, step: p.step, choice: p.choice, sendBackTo: p.sendBackTo, note: p.note })
      } else {
        step.status = 'passed'
        run.progress.done += 1
        const next = run.steps.find((s) => s.id === 'writing')!
        if (p.step === 'review') next.status = 'pending'
        run.state = 'running'
        run.decisions.push({ at: when, step: p.step, choice: p.choice, note: p.note })
      }
      needs = needs.filter((n) => !(n.source.kind === 'run' && n.source.step === p.step))
      post('run.changed', { dir: run.dir })
      post('needs.changed', { count: needs.length })
      post('overview.changed')
    },
    'run.pause': () => {
      run.state = 'paused'
      post('run.changed', { dir: run.dir })
      post('overview.changed')
    },
    'run.resume': () => {
      run.state = 'running'
      post('run.changed', { dir: run.dir })
      post('overview.changed')
    },
    'run.cancel': () => {
      run.state = 'cancelled'
      post('run.changed', { dir: run.dir })
      post('overview.changed')
    },
    'run.retry': (p) => {
      const s = run.steps.find((x) => x.id === p.step)
      if (s) s.status = 'running'
      post('run.changed', { dir: run.dir })
    },
    'need.resolve': (p) => {
      needs = needs.filter((n) => n.id !== p.id)
      post('needs.changed', { count: needs.length })
      post('overview.changed')
    },
    'board.open': () => undefined,
    'run.open': () => undefined,
    'link.read': (p) => {
      const j = JSON.parse(p.text) as { project?: string; run?: string }
      return j.run ? { kind: 'run', dir: j.run } : { kind: 'board', root: j.project ?? fx.ROOT }
    },
  }

  const call = async (name: string, params: unknown) => {
    const h = handlers[name]
    if (!h) throw Object.assign(new Error(`unknown request ${name}`), { code: 'unsupported' })
    const out = await delay(h(params ?? {}), opts.scenario === 'slow' ? 900 : 40)
    return out
  }

  // ---- document (for editors) ----
  let docText = opts.doc ?? ''
  let version = 1
  const docSubs = new Set<(d: any) => void>()
  const docApi: NonNullable<OrcaFrame['document']> = {
    get: async () => ({ version, readOnly: false, content: { kind: 'text', text: docText } }),
    onDidChange: (cb) => {
      docSubs.add(cb)
      return { dispose: () => docSubs.delete(cb) }
    },
    applyEdit: async (e) => {
      docText = e.content.text
      version++
    },
    session: async <T,>(o: { decode?: (c: any) => T; encode?: (v: T) => { kind: 'text'; text: string } }) => {
      let value = (o.decode ? o.decode({ kind: 'text', text: docText }) : (docText as unknown)) as T
      const rep = new Set<(v: T) => void>()
      return {
        get value() {
          return value
        },
        readOnly: false,
        onReplace: (cb: (v: T) => void) => {
          rep.add(cb)
          return { dispose: () => rep.delete(cb) }
        },
        change: (v: T) => {
          value = v
          docText = o.encode ? o.encode(v).text : String(v)
          version++
        },
        flush: async () => {},
      }
    },
  }

  const orca: OrcaFrame = {
    api: { major: 1, minor: 10 },
    locale: opts.lang === 'zh' ? 'zh-CN' : 'en-US',
    host: { call: async (_m, p) => ({ output: await call((p as { name: string }).name, (p as { input: unknown }).input) }), has: () => true },
    on: (name, cb) => {
      if (!subs.has(name)) subs.set(name, new Set())
      subs.get(name)!.add(cb)
      return { dispose: () => subs.get(name)?.delete(cb) }
    },
    plugin: {
      request: (name, input) => call(name, input),
      on: (name, cb) => {
        if (!subs.has(name)) subs.set(name, new Set())
        subs.get(name)!.add(cb)
        return { dispose: () => subs.get(name)?.delete(cb) }
      },
    },
    document: docApi,
  }
  window.orca = orca

  // Light "live" motion so the dev page feels alive: the robustness step reports now and then.
  return { post }
}
