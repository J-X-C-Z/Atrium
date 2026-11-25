// The run driver: ask the core what a run needs next and do it — launch agents, run checks,
// raise gates. All run state lives in the run folder through the core, so a restart resumes.

import { existsSync } from 'node:fs'
import { basename, join } from 'node:path'
import { normalizeEffects, type CheckSpec, type RunEffect } from '../coreapi.js'
import { HostError } from '../host.js'
import type { RunSummary, RunView, StepView } from '../protocol.js'
import type { Binding, RunMeta } from '../state.js'
import { T, reviewPrompt, stepFooter } from '../i18n.js'
import { safeName } from '../workspaces.js'
import { readVerdict, runCheck } from './checks.js'
import { errorText, type Ctx } from './context.js'

const key = (dir: string, step: string, item?: string) => `${dir}\u0000${step}\u0000${item ?? ''}`
const FINAL = new Set(['done', 'failed', 'cancelled'])

export interface StandInSource {
  /** A stand-in agent for work in `dir` whose agent hit a limit, if a project team covers it. */
  standInFor(dir: string, binding: Binding): { agent: string; member: string } | null
}

export class Runs {
  private chains = new Map<string, Promise<void>>()
  private inflight = new Set<string>()
  /** Launches that failed: not retried until the person asks. */
  private failed = new Set<string>()
  /** Agent overrides after a stand-in pick. */
  private agentFor = new Map<string, string>()
  private finalCache = new Map<string, string>()
  standIns: StandInSource | null = null

  constructor(private ctx: Ctx) {}

  metas(): RunMeta[] {
    return this.ctx.store.data.runs
  }

  meta(dir: string): RunMeta | undefined {
    return this.ctx.store.data.runs.find((r) => r.dir === dir)
  }

  private requireMeta(dir: string): RunMeta {
    const meta = this.meta(dir)
    if (!meta) throw new HostError('invalid_params', `unknown run ${dir}`)
    return meta
  }

  // ---------- start ----------

  async start(params: { templateId: string; inputs: Record<string, string>; workspaceId?: string }): Promise<{ dir: string }> {
    const ws = params.workspaceId ? await this.ctx.workspaces.byId(params.workspaceId) : await this.ctx.workspaces.focusedWorkspace()
    if (!ws) throw new HostError('invalid_params', T.noWorkspace(this.ctx.lang()))
    const templates = this.ctx.core.call<{ id: string; path?: string }[]>('workflow.templates', { dirs: this.ctx.templateDirs() })
    const template = (Array.isArray(templates) ? templates : []).find((t) => t.id === params.templateId)
    const dir = freeDir(join(ws.path, 'atrium-runs'), `${safeName(params.templateId)}-${stamp(new Date(this.ctx.now()))}`)
    this.ctx.core.call('workflow.run.start', {
      template: params.templateId,
      ...(template?.path ? { path: template.path } : {}),
      dirs: this.ctx.templateDirs(),
      inputs: params.inputs ?? {},
      dir
    })
    this.ctx.store.data.runs.unshift({ dir, workspaceId: ws.id, templateId: params.templateId })
    this.ctx.store.save()
    this.ctx.host.post('overview.changed')
    void this.pump(dir)
    return { dir }
  }

  // ---------- the loop ----------

  /** One pass over what the run needs, serialized per run. */
  pump(dir: string): Promise<void> {
    const prev = this.chains.get(dir) ?? Promise.resolve()
    const next = prev.then(() => this.pass(dir)).catch((error) => this.ctx.host.log(`runs: ${dir}: ${errorText(error)}`))
    this.chains.set(dir, next)
    return next
  }

  /** Every run that is not finished. */
  async tick(): Promise<void> {
    await Promise.all(this.metas().filter((m) => !this.finalCache.has(m.dir)).map((m) => this.pump(m.dir)))
  }

  /** Resolves once every queued pass and started launch/check has settled (for tests). */
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

  private async pass(dir: string): Promise<void> {
    const meta = this.meta(dir)
    if (!meta) return
    let effects: RunEffect[]
    try {
      effects = normalizeEffects(this.ctx.core.call('workflow.run.next', { dir, lang: this.ctx.lang(), maxParallel: this.ctx.settings().maxParallel }))
    } catch (error) {
      this.raiseError(dir, undefined, errorText(error))
      return
    }
    for (const effect of effects) {
      switch (effect.kind) {
        case 'launch':
          this.maybeLaunch(meta, effect)
          break
        case 'check':
          this.maybeCheck(meta, effect)
          break
        case 'gate':
          this.raiseGate(meta, effect)
          break
        case 'done':
          this.finalCache.set(dir, effect.state ?? 'done')
          this.ctx.needs.removeWhere((n) => n.source.kind === 'run' && n.source.dir === dir && n.kind === 'gate')
          break
        case 'wait':
          break
      }
    }
    this.ctx.host.post('run.changed', { dir })
    this.ctx.host.post('overview.changed')
  }

  private maybeLaunch(meta: RunMeta, e: Extract<RunEffect, { kind: 'launch' }>): void {
    const k = key(meta.dir, e.step, e.item)
    if (this.inflight.has(k) || this.failed.has(k) || this.ctx.store.forStep(meta.dir, e.step, e.item)) return
    if (this.ctx.agents.running() >= this.ctx.settings().maxParallel) return
    this.inflight.add(k)
    void this.launchStep(meta, e)
      .then(() => {
        this.inflight.delete(k)
      })
      .catch((error) => {
        this.inflight.delete(k)
        this.failed.add(k)
        this.raiseError(meta.dir, e.step, errorText(error))
      })
      .finally(() => void this.pump(meta.dir))
  }

  private async launchStep(meta: RunMeta, e: Extract<RunEffect, { kind: 'launch' }>): Promise<Binding> {
    const k = key(meta.dir, e.step, e.item)
    let workspaceId = meta.workspaceId
    if (e.worktree) {
      const base = await this.ctx.workspaces.byId(meta.workspaceId)
      if (!base) throw new Error(`workspace ${meta.workspaceId} is gone`)
      workspaceId = await this.ctx.workspaces.createWorktree(base, `${meta.templateId}-${e.step}${e.item ? `-${e.item}` : ''}`)
    }
    const agent = this.agentFor.get(k) ?? e.agent ?? this.ctx.settings().defaultAgent
    const prompt = `${e.prompt}\n${stepFooter(this.ctx.lang(), { dir: meta.dir, step: e.step, ...(e.item ? { item: e.item } : {}), cli: this.ctx.cli })}`
    return this.ctx.agents.launch({
      workspaceId,
      agent,
      prompt,
      binding: { kind: 'step', dir: meta.dir, step: e.step, ...(e.item ? { item: e.item } : {}) },
      env: { ATRIUM_RUN: meta.dir, ATRIUM_STEP: e.step, ...(e.item ? { ATRIUM_ITEM: e.item } : {}) }
    })
  }

  private maybeCheck(meta: RunMeta, e: Extract<RunEffect, { kind: 'check' }>): void {
    const k = `check\u0000${key(meta.dir, e.step, e.item)}\u0000${e.index ?? ''}`
    if (this.inflight.has(k) || this.failed.has(k)) return
    if (e.check.kind === 'review') {
      const open = this.ctx.store.live().some((b) => b.kind === 'review' && b.dir === meta.dir && b.step === e.step && (b.item ?? '') === (e.item ?? ''))
      if (open) return
      this.inflight.add(k)
      void this.launchReview(meta, e)
        .catch((error) => this.sendCheck(meta.dir, e, { what: 'review', passed: false, detail: errorText(error) }))
        .finally(() => this.inflight.delete(k))
      return
    }
    this.inflight.add(k)
    void runCheck(meta.dir, e.check)
      .catch((error) => ({ what: String(e.check.kind), passed: false, detail: errorText(error) }))
      .then((result) => {
        if (!this.sendCheck(meta.dir, e, result)) this.failed.add(k)
      })
      .finally(() => {
        this.inflight.delete(k)
        void this.pump(meta.dir)
      })
  }

  private sendCheck(dir: string, e: { step: string; item?: string; index?: number }, result: { what: string; passed: boolean; detail: string; log?: string }): boolean {
    try {
      this.ctx.core.call('workflow.run.check_result', {
        dir,
        step: e.step,
        ...(e.item ? { item: e.item } : {}),
        ...(e.index !== undefined ? { index: e.index } : {}),
        lang: this.ctx.lang(),
        result
      })
      return true
    } catch (error) {
      this.raiseError(dir, e.step, errorText(error))
      return false
    }
  }

  private async launchReview(meta: RunMeta, e: Extract<RunEffect, { kind: 'check' }>): Promise<void> {
    const check = e.check as CheckSpec & { rubric?: string; files?: string[]; agent?: string; prompt?: string }
    const lang = this.ctx.lang()
    const prompt =
      (typeof check.prompt === 'string' && check.prompt) ||
      reviewPrompt(lang, String(check.rubric ?? ''), Array.isArray(check.files) ? check.files.map(String) : [], meta.dir)
    await this.ctx.agents.launch({
      workspaceId: meta.workspaceId,
      agent: typeof check.agent === 'string' && check.agent ? check.agent : this.ctx.settings().defaultAgent,
      prompt: `${prompt}\n${stepFooter(lang, { dir: meta.dir, step: e.step, ...(e.item ? { item: e.item } : {}), cli: this.ctx.cli })}`,
      binding: { kind: 'review', dir: meta.dir, step: e.step, ...(e.item ? { item: e.item } : {}), ...(e.index !== undefined ? { checkIndex: e.index } : {}) },
      env: { ATRIUM_RUN: meta.dir, ATRIUM_STEP: e.step, ATRIUM_REVIEW: '1', ...(e.item ? { ATRIUM_ITEM: e.item } : {}) }
    })
  }

  private raiseGate(meta: RunMeta, e: Extract<RunEffect, { kind: 'gate' }>): void {
    const id = `gate:${meta.dir}:${e.step}`
    if (this.ctx.needs.get(id)) return
    const name = this.templateName(meta)
    this.ctx.needs.add({
      id,
      kind: 'gate',
      title: e.title ?? T.gate(this.ctx.lang(), `${name} · ${e.step}`),
      ...(e.detail ? { detail: e.detail } : {}),
      createdAt: this.ctx.now(),
      source: { kind: 'run', dir: meta.dir, step: e.step },
      choices: e.choices.length ? e.choices : [{ id: 'approve', label: this.ctx.lang() === 'zh' ? '通过' : 'Approve' }],
      sendBackTargets: e.sendBack
    })
  }

  private raiseError(dir: string, step: string | undefined, message: string): void {
    this.ctx.host.log(`runs: ${dir}${step ? ` ${step}` : ''}: ${message}`)
    const meta = this.meta(dir)
    this.ctx.needs.add({
      id: `error:${dir}:${step ?? ''}`,
      kind: 'error',
      title: T.error(this.ctx.lang(), `${meta ? this.templateName(meta) : basename(dir)}${step ? ` · ${step}` : ''}`),
      detail: message,
      createdAt: this.ctx.now(),
      source: { kind: 'run', dir, ...(step ? { step } : {}) }
    })
  }

  private templateName(meta: RunMeta): string {
    try {
      const view = this.ctx.core.call<Partial<RunView>>('workflow.run.load', { dir: meta.dir, lang: this.ctx.lang() })
      return view?.templateName || meta.templateId
    } catch {
      return meta.templateId
    }
  }

  // ---------- agents talking back ----------

  /** `atrium report` from a step or review agent. */
  async report(binding: Binding, text: string): Promise<string> {
    const dir = binding.dir!
    const step = binding.step!
    this.ctx.agents.reported(binding)
    if (binding.kind === 'review') {
      const verdict = readVerdict(text)
      this.sendCheck(dir, { step, ...(binding.item ? { item: binding.item } : {}), ...(binding.checkIndex !== undefined ? { index: binding.checkIndex } : {}) }, {
        what: 'review',
        passed: verdict.passed,
        detail: verdict.detail
      })
    } else {
      this.ctx.core.call('workflow.run.report', { dir, step, ...(binding.item ? { item: binding.item } : {}), answer: text })
    }
    await this.ctx.agents.release(binding, false)
    this.ctx.needs.removeWhere((n) => n.terminalId !== undefined && n.terminalId === binding.terminalId && (n.kind === 'stalled' || n.kind === 'blocked'))
    await this.pump(dir)
    return this.ctx.lang() === 'zh' ? '已收到报告。' : 'Report received.'
  }

  onStalled(binding: Binding): void {
    const meta = binding.dir ? this.meta(binding.dir) : undefined
    const what = `${meta ? this.templateName(meta) : basename(binding.dir ?? '')} · ${binding.step}${binding.item ? ` / ${binding.item}` : ''}`
    this.ctx.needs.add({
      id: `stalled:${binding.terminalId}`,
      kind: 'stalled',
      title: T.stalled(this.ctx.lang(), what),
      createdAt: this.ctx.now(),
      source: { kind: 'run', dir: binding.dir!, ...(binding.step ? { step: binding.step } : {}) },
      ...(binding.terminalId ? { terminalId: binding.terminalId } : {})
    })
  }

  onBlocked(binding: Binding): void {
    this.ctx.needs.add({
      id: `blocked:${binding.terminalId}`,
      kind: 'blocked',
      title: T.blocked(this.ctx.lang(), `${binding.step}${binding.item ? ` / ${binding.item}` : ''}`),
      createdAt: this.ctx.now(),
      source: { kind: 'run', dir: binding.dir!, ...(binding.step ? { step: binding.step } : {}) },
      ...(binding.terminalId ? { terminalId: binding.terminalId } : {})
    })
  }

  /** The terminal went away before the agent reported: start the step again. */
  onGone(binding: Binding): void {
    if (binding.reported || !binding.dir || !binding.step) return
    this.retryInCore(binding.dir, binding.step, binding.item)
    void this.pump(binding.dir)
  }

  /** Usage limit: a stand-in from the project's team, else ask the person. */
  async onQuota(binding: Binding, evidence: string): Promise<void> {
    if (!binding.dir || !binding.step) return
    const pick = this.standIns?.standInFor(binding.dir, binding) ?? null
    this.ctx.needs.add({
      id: `quota:${binding.terminalId}`,
      kind: 'quota',
      title: T.quota(this.ctx.lang(), binding.member ?? binding.agent, pick?.member),
      detail: evidence,
      createdAt: this.ctx.now(),
      source: { kind: 'run', dir: binding.dir, step: binding.step },
      ...(binding.terminalId ? { terminalId: binding.terminalId } : {})
    })
    if (!pick) return
    this.agentFor.set(key(binding.dir, binding.step, binding.item), pick.agent)
    await this.ctx.agents.release(binding, false)
    this.retryInCore(binding.dir, binding.step, binding.item)
    await this.pump(binding.dir)
  }

  private retryInCore(dir: string, step: string, item?: string): void {
    try {
      this.ctx.core.call('workflow.run.retry', { dir, step, ...(item ? { item } : {}) })
    } catch (error) {
      this.ctx.host.log(`runs: retry ${step} in ${dir}: ${errorText(error)}`)
    }
  }

  // ---------- requests ----------

  async decide(p: { dir: string; step: string; choice: string; sendBackTo?: string; note?: string }): Promise<void> {
    this.requireMeta(p.dir)
    this.ctx.core.call('workflow.run.decide', {
      dir: p.dir,
      step: p.step,
      choice: p.choice,
      ...(p.sendBackTo ? { send_back_to: p.sendBackTo } : {}),
      ...(p.note ? { note: p.note } : {})
    })
    this.ctx.needs.remove(`gate:${p.dir}:${p.step}`)
    await this.pump(p.dir)
  }

  async control(dir: string, op: 'pause' | 'resume' | 'cancel'): Promise<void> {
    this.requireMeta(dir)
    this.ctx.core.call(`workflow.run.${op}`, { dir })
    if (op === 'cancel') {
      for (const b of this.ctx.store.live().filter((b) => b.dir === dir && b.kind !== 'role')) await this.ctx.agents.release(b, true)
      this.ctx.needs.removeWhere((n) => n.source.kind === 'run' && n.source.dir === dir)
    }
    if (op === 'resume') this.finalCache.delete(dir)
    await this.pump(dir)
  }

  async retry(p: { dir: string; step: string; item?: string }): Promise<void> {
    this.requireMeta(p.dir)
    const k = key(p.dir, p.step, p.item)
    this.failed.delete(k)
    for (const f of [...this.failed]) if (f.startsWith(`check\u0000${k}`)) this.failed.delete(f)
    const old = this.ctx.store.forStep(p.dir, p.step, p.item)
    if (old) await this.ctx.agents.release(old, false)
    this.ctx.core.call('workflow.run.retry', { dir: p.dir, step: p.step, ...(p.item ? { item: p.item } : {}) })
    this.ctx.needs.removeWhere((n) => n.source.kind === 'run' && n.source.dir === p.dir && n.source.step === p.step && n.kind !== 'gate')
    this.finalCache.delete(p.dir)
    await this.pump(p.dir)
  }

  view(dir: string): RunView {
    const meta = this.requireMeta(dir)
    const raw = this.ctx.core.call<Partial<RunView>>('workflow.run.load', { dir, lang: this.ctx.lang() })
    const steps: StepView[] = (raw.steps ?? []).map((s) => {
      const items = s.items
      const bound = this.ctx.store.forStep(dir, s.id) ?? this.ctx.store.live().find((b) => b.dir === dir && b.step === s.id)
      return {
        ...s,
        needs: s.needs ?? [],
        attempts: s.attempts ?? [],
        ...(items ? { items } : {}),
        ...(s.terminalId || bound?.terminalId ? { terminalId: s.terminalId ?? bound!.terminalId! } : {})
      }
    })
    const view: RunView = {
      dir,
      templateId: raw.templateId ?? meta.templateId,
      templateName: raw.templateName ?? meta.templateId,
      state: raw.state ?? 'running',
      startedAt: raw.startedAt ?? 0,
      progress: raw.progress ?? { done: steps.filter((s) => s.status === 'passed' || s.status === 'skipped').length, total: steps.length },
      ...(raw.currentStep ? { currentStep: raw.currentStep } : {}),
      inputs: raw.inputs ?? {},
      steps,
      decisions: raw.decisions ?? [],
      experience: raw.experience ?? []
    }
    if (FINAL.has(view.state)) this.finalCache.set(dir, view.state)
    return view
  }

  summary(dir: string): RunSummary | null {
    try {
      const v = this.view(dir)
      return {
        dir: v.dir,
        templateId: v.templateId,
        templateName: v.templateName,
        state: v.state,
        startedAt: v.startedAt,
        progress: v.progress,
        ...(v.currentStep ? { currentStep: v.currentStep } : {})
      }
    } catch (error) {
      this.ctx.host.log(`runs: cannot load ${dir}: ${errorText(error)}`)
      return null
    }
  }
}

function stamp(d: Date): string {
  const p = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}`
}

function freeDir(parent: string, name: string): string {
  let dir = join(parent, name)
  for (let n = 2; existsSync(dir); n++) dir = join(parent, `${name}-${n}`)
  return dir
}
