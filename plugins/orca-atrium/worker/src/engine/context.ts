// What every engine part shares.

import type { CoreApi } from '../coreapi.js'
import type { Host } from '../host.js'
import type { Lang } from '../protocol.js'
import type { Settings } from '../settings.js'
import type { Store } from '../state.js'
import type { Workspaces } from '../workspaces.js'
import type { Agents } from './agents.js'
import type { Needs } from './needs.js'

export interface Ctx {
  host: Host
  core: CoreApi
  store: Store
  agents: Agents
  needs: Needs
  workspaces: Workspaces
  settings(): Settings
  lang(): Lang
  /** The CLI as a full command, for agents whose PATH lost our bin folder. */
  cli: string
  /** Template folders: the user's, then each known project's. */
  templateDirs(): string[]
  now(): number
}

export function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
