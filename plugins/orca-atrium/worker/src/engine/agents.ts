// Agents we launched: start them, follow agent.status.changed, nudge or flag the ones that stop
// without reporting, and spot usage limits so a stand-in can take over.

import { randomUUID } from 'node:crypto'
import type { Host } from '../host.js'
import type { AgentState, Lang, LiveAgent } from '../protocol.js'
import type { Binding, Store } from '../state.js'
import type { LaunchHook } from '../bridge/hook.js'
import { nudgeText } from '../i18n.js'

export interface LaunchRequest {
  workspaceId: string
  agent: string
  prompt: string
  binding: Omit<Binding, 'launchId' | 'terminalId' | 'workspaceId' | 'agent' | 'startedAt' | 'reported' | 'nudged'>
  /** Extra ATRIUM_* variables for the agent's env. */
  env: Record<string, string>
}

export interface AgentOptions {
  /** After a turn ends with no report: wait this long, nudge, wait again, then flag it. */
  graceMs: number
  socket: string
  token: string
}

export interface AgentEvents {
  /** A bound agent's state changed. */
  changed?(binding: Binding | undefined, state: AgentState, terminalId: string): void
  /** A step or review agent stopped twice without reporting. */
  stalled?(binding: Binding): void
  /** Orca says the agent waits on the person (a permission prompt). */
  blocked?(binding: Binding): void
  /** A turn ended (any kind). */
  idle?(binding: Binding): void
  /** The agent hit a usage limit. */
  quota?(binding: Binding, evidence: string): void
  /** The terminal is gone (closed, or lost across a restart). */
  gone?(binding: Binding): void
}

const QUOTA = [
  /usage limit/i,
  /rate[- ]limit(ed)?\b.*(reached|exceeded|hit)/i,
  /(hit|reached) (your|the) (usage |daily |weekly )?limit/i,
  /quota (exceeded|exhausted|reached)/i,
  /out of (credits|tokens)/i,
  /limit (will )?resets? (at|in)/i,
  /用量(已)?(达到)?上限/,
  /额度(已)?(用完|耗尽|不足)/
]

export function quotaEvidence(lines: string[]): string | null {
  const tail = lines.slice(-40)
  for (const line of tail.reverse()) {
    if (QUOTA.some((re) => re.test(line))) return line.trim().slice(0, 300)
  }
  return null
}

export class Agents {
  private liveMap = new Map<string, LiveAgent>()
  private timers = new Map<string, NodeJS.Timeout>()
  private launching: Promise<unknown> = Promise.resolve()
  events: AgentEvents = {}

  constructor(
    private host: Host,
    private store: Store,
    private hook: LaunchHook,
    private opts: AgentOptions,
    private lang: () => Lang
  ) {}

  list(): LiveAgent[] {
    return [...this.liveMap.values()]
  }

  stateOf(terminalId: string | null | undefined): AgentState | undefined {
    return terminalId ? this.liveMap.get(terminalId)?.state : undefined
  }

  /** Bound agents still alive. */
  running(): number {
    return this.store.live().length
  }

  /** Starts an agent with our env. One launch at a time, so the hook matches the right one. */
  launch(req: LaunchRequest): Promise<Binding> {
    const run = this.launching.then(() => this.launchNow(req))
    this.launching = run.catch(() => {})
    return run
  }

  private async launchNow(req: LaunchRequest): Promise<Binding> {
    const launchId = randomUUID()
    const env: Record<string, string> = {
      ATRIUM_SOCKET: this.opts.socket,
      ATRIUM_TOKEN: this.opts.token,
      ATRIUM_LAUNCH: launchId,
      ...req.env
    }
    const pending = this.hook.expect({ launchId, workspaceId: req.workspaceId, agent: req.agent, env })
    let terminalId: string
    try {
      const answer = await this.host.call<{ terminalId: string }>('agent.launch', {
        workspaceId: req.workspaceId,
        agent: req.agent,
        prompt: req.prompt
      })
      terminalId = answer.terminalId
    } finally {
      this.hook.forget(launchId)
    }
    if (!pending.matched) this.host.log(`agents: launch ${launchId} in ${req.workspaceId} did not pass our hook`)
    const binding: Binding = {
      ...req.binding,
      launchId,
      terminalId,
      workspaceId: req.workspaceId,
      agent: req.agent,
      startedAt: Date.now(),
      reported: false,
      nudged: false
    }
    this.store.addBinding(binding)
    this.setLive(terminalId, { workspaceId: req.workspaceId, agent: req.agent, state: 'starting', binding })
    return binding
  }

  private setLive(
    terminalId: string,
    patch: { workspaceId?: string | null; agent?: string; title?: string; state: AgentState; binding?: Binding }
  ): void {
    const old = this.liveMap.get(terminalId)
    const binding = patch.binding ?? this.store.byTerminal(terminalId)
    const live: LiveAgent = {
      terminalId,
      workspaceId: patch.workspaceId ?? old?.workspaceId ?? binding?.workspaceId ?? null,
      agent: patch.agent ?? old?.agent ?? binding?.agent ?? 'agent',
      ...(patch.title ?? old?.title ? { title: patch.title ?? old?.title } : {}),
      state: patch.state,
      ...(binding ? { bound: boundOf(binding) } : {}),
      since: old && old.state === patch.state ? old.since : Date.now()
    }
    this.liveMap.set(terminalId, live)
    if (!old || old.state !== patch.state) {
      this.host.post('agent.changed', { terminalId, state: patch.state })
      this.events.changed?.(binding, patch.state, terminalId)
    }
  }

  /** `agent.status.changed`. */
  async onStatus(payload: {
    terminalId?: string
    paneKey?: string
    workspaceId?: string | null
    title?: string
    state: 'working' | 'blocked' | 'waiting' | 'done'
  }): Promise<void> {
    const terminalId = payload.terminalId
    if (!terminalId) return
    const binding = this.store.byTerminal(terminalId)
    this.setLive(terminalId, {
      workspaceId: payload.workspaceId ?? null,
      ...(payload.title ? { title: payload.title } : {}),
      state: payload.state
    })
    if (!binding) return
    this.clearTimer(terminalId)
    if (payload.state === 'working') return
    if (await this.checkQuota(binding)) return
    if (payload.state === 'blocked') {
      this.events.blocked?.(binding)
      return
    }
    this.events.idle?.(binding)
    if (binding.kind !== 'role' && !binding.reported) this.armNudge(binding)
  }

  private armNudge(binding: Binding): void {
    const terminalId = binding.terminalId
    if (!terminalId) return
    this.timers.set(
      terminalId,
      setTimeout(() => void this.afterGrace(binding), this.opts.graceMs)
    )
  }

  private async afterGrace(binding: Binding): Promise<void> {
    if (!binding.terminalId) return
    this.timers.delete(binding.terminalId)
    if (binding.reported || binding.ended) return
    const state = this.stateOf(binding.terminalId)
    if (state === 'working' || state === 'starting') return
    if (!binding.nudged) {
      binding.nudged = true
      this.store.save()
      await this.nudge(binding.terminalId, binding.kind === 'role' ? 'role' : 'step')
      // A nudge that starts no turn still ends in a stall.
      this.armNudge(binding)
      return
    }
    this.setLive(binding.terminalId, { state: 'stalled' })
    this.events.stalled?.(binding)
  }

  async nudge(terminalId: string, kind?: 'step' | 'role'): Promise<void> {
    const binding = this.store.byTerminal(terminalId)
    const text = nudgeText(this.lang(), kind ?? (binding?.kind === 'role' ? 'role' : 'step'))
    await this.send(terminalId, text)
  }

  async send(terminalId: string, text: string): Promise<boolean> {
    try {
      const answer = await this.host.call<{ accepted: boolean }>('terminal.sendText', { terminalId, text, enter: true })
      return answer?.accepted !== false
    } catch (error) {
      this.host.log(`agents: send to ${terminalId}: ${String(error)}`)
      return false
    }
  }

  private async checkQuota(binding: Binding): Promise<boolean> {
    if (!binding.terminalId || !this.host.has('terminal.read')) return false
    let lines: string[] = []
    try {
      lines = (await this.host.call<{ lines: string[] }>('terminal.read', { terminalId: binding.terminalId, maxLines: 80 })).lines ?? []
    } catch {
      return false
    }
    const evidence = quotaEvidence(lines)
    if (!evidence) return false
    if (binding.member) {
      this.store.data.quotas[binding.member] = { ...(this.store.data.quotas[binding.member] ?? {}), limited: true }
      this.store.save()
    }
    this.events.quota?.(binding, evidence)
    return true
  }

  /** The agent reported: no nudge, no stall. */
  reported(binding: Binding): void {
    binding.reported = true
    this.store.save()
    if (binding.terminalId) {
      this.clearTimer(binding.terminalId)
      if (this.stateOf(binding.terminalId) === 'stalled') this.setLive(binding.terminalId, { state: 'done' })
    }
  }

  /** Stops driving this binding; closes the terminal when asked. */
  async release(binding: Binding, close: boolean): Promise<void> {
    this.store.end(binding)
    if (!binding.terminalId) return
    this.clearTimer(binding.terminalId)
    if (close) {
      try {
        await this.host.call('terminal.close', { terminalId: binding.terminalId })
      } catch (error) {
        this.host.log(`agents: close ${binding.terminalId}: ${String(error)}`)
      }
      this.liveMap.delete(binding.terminalId)
      this.host.post('agent.changed', { terminalId: binding.terminalId, state: 'idle' })
    } else {
      const live = this.liveMap.get(binding.terminalId)
      if (live) {
        delete live.bound
      }
    }
  }

  async focus(terminalId: string): Promise<void> {
    await this.host.call('terminal.focus', { terminalId })
  }

  /** After a restart: keep bindings whose terminals Orca still lists, drop the rest. */
  async reconcile(): Promise<void> {
    const byWorkspace = new Map<string, Binding[]>()
    for (const b of this.store.live()) {
      const list = byWorkspace.get(b.workspaceId) ?? []
      list.push(b)
      byWorkspace.set(b.workspaceId, list)
    }
    for (const [workspaceId, bindings] of byWorkspace) {
      let agents: { terminalId: string | null; title: string | null; agent: string | null; state: AgentState }[]
      try {
        agents = (await this.host.call<{ agents: typeof agents }>('agents.list', { workspaceId })).agents ?? []
      } catch (error) {
        // Unknown workspace (removed) means those agents are gone; anything else, keep waiting.
        const code = (error as { code?: string }).code
        if (code !== 'invalid_params' && code !== 'action_failed') continue
        agents = []
      }
      for (const b of bindings) {
        const found = agents.find((a) => a.terminalId === b.terminalId)
        if (found && b.terminalId) {
          this.setLive(b.terminalId, { workspaceId, agent: found.agent ?? b.agent, ...(found.title ? { title: found.title } : {}), state: found.state, binding: b })
          if (found.state !== 'working' && b.kind !== 'role' && !b.reported) this.armNudge(b)
        } else {
          this.store.end(b)
          this.events.gone?.(b)
        }
      }
    }
  }

  private clearTimer(terminalId: string): void {
    const timer = this.timers.get(terminalId)
    if (timer) clearTimeout(timer)
    this.timers.delete(terminalId)
  }

  dispose(): void {
    for (const timer of this.timers.values()) clearTimeout(timer)
    this.timers.clear()
  }
}

export function boundOf(b: Binding): LiveAgent['bound'] {
  if (b.kind === 'role') return { kind: 'role', root: b.root ?? '', role: b.role ?? '', ...(b.task ? { task: b.task } : {}) }
  return { kind: 'step', dir: b.dir ?? '', step: b.step ?? '', ...(b.item ? { item: b.item } : {}) }
}
