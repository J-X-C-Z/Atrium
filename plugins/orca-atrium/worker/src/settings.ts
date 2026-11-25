// The plugin's declared settings (contributes.settings), read with defaults.

import type { Host } from './host.js'

export interface Settings {
  language: 'auto' | 'zh' | 'en'
  defaultAgent: string
  maxParallel: number
  autopilotDefault: boolean
}

export const DEFAULTS: Settings = { language: 'auto', defaultAgent: 'claude', maxParallel: 4, autopilotDefault: false }

export function parseSettings(raw: Record<string, unknown> | null | undefined): Settings {
  const r = raw ?? {}
  const language = r.language === 'zh' || r.language === 'en' ? r.language : 'auto'
  const defaultAgent = typeof r.defaultAgent === 'string' && r.defaultAgent.trim() ? r.defaultAgent.trim() : DEFAULTS.defaultAgent
  const maxParallel =
    typeof r.maxParallel === 'number' && Number.isFinite(r.maxParallel) && r.maxParallel >= 1 ? Math.floor(r.maxParallel) : DEFAULTS.maxParallel
  const autopilotDefault = typeof r.autopilotDefault === 'boolean' ? r.autopilotDefault : DEFAULTS.autopilotDefault
  return { language, defaultAgent, maxParallel, autopilotDefault }
}

export async function readSettings(host: Host): Promise<Settings> {
  try {
    return parseSettings(await host.settings())
  } catch {
    return { ...DEFAULTS }
  }
}
