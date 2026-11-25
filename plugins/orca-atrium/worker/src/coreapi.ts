// What the worker needs from the wasm core (worker/src/core.ts implements it).

export interface CoreApi {
  call<T = unknown>(op: string, args: unknown): T
}

/** Calls one op; a thrown core error keeps its message. */
export function coreCall<T>(core: CoreApi, op: string, args: unknown): T {
  return core.call<T>(op, args)
}

// ---------- run effects (`workflow.run.next`) ----------

export interface CheckSpec {
  kind: string
  [key: string]: unknown
}

export type RunEffect =
  | { kind: 'launch'; step: string; item?: string; prompt: string; agent?: string; worktree?: boolean }
  | { kind: 'check'; step: string; item?: string; index?: number; check: CheckSpec }
  | {
      kind: 'gate'
      step: string
      title?: string
      detail?: string
      choices: { id: string; label: string }[]
      sendBack: { step: string; label: string }[]
    }
  | { kind: 'done'; state?: string }
  | { kind: 'wait' }

const str = (value: unknown): string | undefined => (typeof value === 'string' && value ? value : undefined)

function kindOf(raw: Record<string, unknown>): string {
  return String(raw.kind ?? raw.type ?? raw.effect ?? '')
}

function labelled(list: unknown, key: 'id' | 'step'): { id: string; label: string }[] {
  if (!Array.isArray(list)) return []
  return list.map((entry) => {
    if (typeof entry === 'string') return { id: entry, label: entry }
    const obj = entry as Record<string, unknown>
    const id = String(obj[key] ?? obj.id ?? obj.step ?? '')
    return { id, label: String(obj.label ?? obj.title ?? id) }
  })
}

/** Accepts `[...]` or `{effects: [...]}` and the field spellings the core may use. */
export function normalizeEffects(raw: unknown): RunEffect[] {
  const list = Array.isArray(raw)
    ? raw
    : raw && typeof raw === 'object' && Array.isArray((raw as { effects?: unknown }).effects)
      ? (raw as { effects: unknown[] }).effects
      : raw && typeof raw === 'object'
        ? [raw]
        : []
  const out: RunEffect[] = []
  for (const entry of list) {
    if (!entry || typeof entry !== 'object') continue
    const e = entry as Record<string, unknown>
    const step = String(e.step ?? '')
    const item = str(e.item)
    switch (kindOf(e)) {
      case 'launch':
        out.push({
          kind: 'launch',
          step,
          ...(item ? { item } : {}),
          prompt: String(e.prompt ?? ''),
          ...(str(e.agent) ? { agent: str(e.agent)! } : {}),
          ...(e.worktree === true ? { worktree: true } : {})
        })
        break
      case 'check': {
        const nested = e.check && typeof e.check === 'object' ? (e.check as Record<string, unknown>) : null
        const check: CheckSpec = nested
          ? { ...nested, kind: String(nested.kind ?? nested.type ?? '') }
          : { ...e, kind: String(e.check_kind ?? e.checkKind ?? '') }
        out.push({
          kind: 'check',
          step,
          ...(item ? { item } : {}),
          ...(typeof e.index === 'number' ? { index: e.index } : {}),
          check
        })
        break
      }
      case 'gate': {
        const sendBack = labelled(e.send_back ?? e.sendBack ?? e.fixes, 'step').map((x) => ({ step: x.id, label: x.label }))
        out.push({
          kind: 'gate',
          step,
          ...(str(e.title) ? { title: str(e.title)! } : {}),
          ...(str(e.detail ?? e.prompt) ? { detail: str(e.detail ?? e.prompt)! } : {}),
          choices: labelled(e.choices, 'id'),
          sendBack
        })
        break
      }
      case 'done':
        out.push({ kind: 'done', ...(str(e.state) ? { state: str(e.state)! } : {}) })
        break
      default:
        out.push({ kind: 'wait' })
    }
  }
  return out
}

// ---------- project decisions (`project.next`) ----------

export type ProjectAction =
  | { kind: 'launch'; role: string; member?: string; agent?: string; model?: string; prompt?: string; task?: string; worktree?: boolean; cwd?: string; standingInFor?: string }
  | { kind: 'send'; role: string; text: string }
  | { kind: 'stop'; role: string }
  | { kind: 'need'; title: string; detail?: string; role?: string; needKind?: string; task?: string }

export function normalizeActions(raw: unknown): ProjectAction[] {
  const list = Array.isArray(raw)
    ? raw
    : raw && typeof raw === 'object'
      ? ((raw as Record<string, unknown>).actions ?? (raw as Record<string, unknown>).effects ?? [])
      : []
  const out: ProjectAction[] = []
  if (!Array.isArray(list)) return out
  for (const entry of list) {
    if (!entry || typeof entry !== 'object') continue
    const e = entry as Record<string, unknown>
    const role = String(e.role ?? e.stream ?? '')
    switch (kindOf(e)) {
      case 'launch':
      case 'start':
        out.push({
          kind: 'launch',
          role,
          ...(str(e.member) ? { member: str(e.member)! } : {}),
          ...(str(e.agent) ? { agent: str(e.agent)! } : {}),
          ...(str(e.model) ? { model: str(e.model)! } : {}),
          ...(str(e.prompt) ? { prompt: str(e.prompt)! } : {}),
          ...(str(e.task) ? { task: str(e.task)! } : {}),
          ...(e.worktree === true ? { worktree: true } : {}),
          ...(str(e.cwd) ? { cwd: str(e.cwd)! } : {}),
          ...(str(e.standingInFor) ? { standingInFor: str(e.standingInFor)! } : {})
        })
        break
      case 'send':
      case 'wake':
      case 'message':
        out.push({ kind: 'send', role, text: String(e.text ?? e.prompt ?? '') })
        break
      case 'stop':
      case 'retire':
        out.push({ kind: 'stop', role })
        break
      case 'need':
      case 'ask':
        out.push({
          kind: 'need',
          title: String(e.title ?? e.text ?? ''),
          ...(str(e.detail) ? { detail: str(e.detail)! } : {}),
          ...(role ? { role } : {}),
          ...(str(e.needKind ?? e.need) ? { needKind: str(e.needKind ?? e.need)! } : {}),
          ...(str(e.task) ? { task: str(e.task)! } : {})
        })
        break
    }
  }
  return out
}

/** `team.pick` answer: who takes the work. */
export interface Pick {
  member: string
  agent?: string
  model?: string
  standingInFor?: string
}

export function normalizePick(raw: unknown): Pick | null {
  if (!raw || typeof raw !== 'object') return null
  const r = raw as Record<string, unknown>
  const inner = (r.pick && typeof r.pick === 'object' ? r.pick : r) as Record<string, unknown>
  const member = str(inner.member ?? inner.name)
  if (!member) return null
  const agent = str(inner.agent)
  const model = str(inner.model)
  const instead = inner.instead_of ?? inner.insteadOf
  const insteadName = Array.isArray(instead) ? instead[0] : instead && typeof instead === 'object' ? (instead as Record<string, unknown>).member : instead
  const standingInFor = str(inner.standing_in_for ?? inner.standingInFor ?? insteadName)
  return { member, ...(agent ? { agent } : {}), ...(model ? { model } : {}), ...(standingInFor ? { standingInFor } : {}) }
}
