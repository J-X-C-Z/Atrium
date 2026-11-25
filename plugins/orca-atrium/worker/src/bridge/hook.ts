// `agent.launch.before`: put `atrium` on PATH and tag the env, only for launches we started.

import { delimiter } from 'node:path'
import type { Host, Disposable } from '../host.js'

export interface PendingLaunch {
  launchId: string
  workspaceId: string
  agent: string
  env: Record<string, string>
  at: number
  /** Set by the hook when Orca names the terminal it is about to start. */
  terminalId?: string
  matched?: boolean
}

export interface LaunchEvent {
  agent: string
  workspaceId: string | null
  workspaceKind?: string | null
  terminalId?: string
  command: string
  env: Record<string, string>
}

/** How long a pending launch waits for its hook. */
const WINDOW_MS = 120_000

export class LaunchHook {
  private pending: PendingLaunch[] = []

  constructor(
    private binDir: string,
    private basePath: () => string | undefined = () => process.env.PATH
  ) {}

  expect(launch: Omit<PendingLaunch, 'at'>): PendingLaunch {
    const entry = { ...launch, at: Date.now() }
    this.pending.push(entry)
    return entry
  }

  forget(launchId: string): void {
    this.pending = this.pending.filter((p) => p.launchId !== launchId)
  }

  /** Finds the oldest launch of ours this event belongs to. */
  match(event: LaunchEvent): PendingLaunch | undefined {
    const now = Date.now()
    this.pending = this.pending.filter((p) => now - p.at < WINDOW_MS)
    const open = this.pending.filter((p) => !p.matched)
    const sameAgent = (p: PendingLaunch) => !event.agent || !p.agent || sameKind(p.agent, event.agent)
    return (
      open.find((p) => event.workspaceId !== null && p.workspaceId === event.workspaceId && sameAgent(p)) ??
      // Orca may not tie the pane to a workspace (null); fall back to the agent kind alone.
      (event.workspaceId === null ? open.find(sameAgent) : undefined)
    )
  }

  /** The hook handler: returns next(event with env) for ours, nothing for others. */
  handle(event: LaunchEvent, next: (event?: unknown) => unknown): unknown {
    const ours = this.match(event)
    if (!ours) return undefined
    ours.matched = true
    if (event.terminalId) ours.terminalId = event.terminalId
    const path = event.env?.PATH ?? this.basePath()
    const env: Record<string, string> = {
      ...(event.env ?? {}),
      ...ours.env,
      PATH: path ? `${this.binDir}${delimiter}${path}` : this.binDir
    }
    return next({ ...event, env })
  }

  register(host: Host): Disposable {
    return host.hook('agent.launch.before', (event, next) => this.handle(event as LaunchEvent, next))
  }
}

function sameKind(a: string, b: string): boolean {
  const norm = (s: string) => s.toLowerCase().replace(/[^a-z0-9]/g, '')
  return norm(a) === norm(b) || norm(a).startsWith(norm(b)) || norm(b).startsWith(norm(a))
}
