// A small TOML reader that keeps source spans, plus text patches for template edits.
// It covers what workflow templates use: tables, arrays of tables, basic/literal/multi-line
// strings, numbers, booleans, arrays and inline tables. Edits splice the original text so
// comments and formatting the person wrote stay put.

export type TomlValue = string | number | boolean | TomlValue[] | { [k: string]: TomlValue }

export interface Entry {
  key: string
  value: TomlValue
  /** Offset of the key's first character. */
  start: number
  /** Offset just past the value. */
  end: number
  line: number
}

export interface Table {
  name: string
  array: boolean
  /** Offset of the `[` of the header (0 for the root table). */
  start: number
  /** Offset just past the header line's newline. */
  bodyStart: number
  /** Offset where the next header starts (or text end). */
  end: number
  line: number
  entries: Entry[]
}

export class TomlError extends Error {
  line: number
  constructor(message: string, line: number) {
    super(message)
    this.line = line
  }
}

export function parseToml(text: string): Table[] {
  let i = 0
  let line = 1
  const n = text.length
  const root: Table = { name: '', array: false, start: 0, bodyStart: 0, end: n, line: 1, entries: [] }
  const tables: Table[] = [root]
  let cur = root

  const err = (m: string): never => {
    throw new TomlError(m, line)
  }
  const peek = (o = 0) => text[i + o]
  const adv = () => {
    if (text[i] === '\n') line++
    i++
  }
  const skipWs = () => {
    while (i < n && (text[i] === ' ' || text[i] === '\t')) i++
  }
  const skipComment = () => {
    if (text[i] === '#') while (i < n && text[i] !== '\n') i++
  }
  const skipWsNl = () => {
    for (;;) {
      skipWs()
      skipComment()
      if (text[i] === '\n' || text[i] === '\r') adv()
      else break
    }
  }
  const bareKey = () => {
    const s = i
    while (i < n && /[A-Za-z0-9_-]/.test(text[i])) i++
    if (s === i) err('expected a key')
    return text.slice(s, i)
  }
  const key = (): string => {
    skipWs()
    let k: string
    if (peek() === '"' || peek() === "'") k = stringVal() as string
    else k = bareKey()
    skipWs()
    if (peek() === '.') {
      i++
      return `${k}.${key()}`
    }
    return k
  }
  const escape = (): string => {
    const c = text[i++]
    switch (c) {
      case 'n':
        return '\n'
      case 't':
        return '\t'
      case 'r':
        return '\r'
      case '"':
        return '"'
      case '\\':
        return '\\'
      case 'b':
        return '\b'
      case 'f':
        return '\f'
      case 'u':
      case 'U': {
        const len = c === 'u' ? 4 : 8
        const hex = text.slice(i, i + len)
        if (!/^[0-9a-fA-F]+$/.test(hex) || hex.length !== len) err('bad unicode escape')
        i += len
        return String.fromCodePoint(parseInt(hex, 16))
      }
      case '\n':
      case ' ':
      case '\t':
      case '\r': {
        // line-ending backslash in multi-line strings
        i--
        while (i < n && /[\s]/.test(text[i])) adv()
        return ''
      }
      default:
        return err(`unknown escape \\${c}`)
    }
  }
  const stringVal = (): string => {
    const q = text[i]
    const multi = text.startsWith(q.repeat(3), i)
    if (multi) {
      i += 3
      if (text[i] === '\r') i++
      if (text[i] === '\n') adv()
      let out = ''
      for (;;) {
        if (i >= n) err('unterminated multi-line string')
        if (text.startsWith(q.repeat(3), i)) {
          i += 3
          // allow up to two quotes right before the closing delimiter
          while (text[i] === q && out.length >= 0 && text.startsWith(q, i)) {
            out += q
            i++
          }
          return out
        }
        if (q === '"' && text[i] === '\\') {
          i++
          out += escape()
          continue
        }
        if (text[i] === '\n') line++
        out += text[i++]
      }
    }
    i++
    let out = ''
    for (;;) {
      if (i >= n || text[i] === '\n') err('unterminated string')
      const c = text[i]
      if (c === q) {
        i++
        return out
      }
      if (q === '"' && c === '\\') {
        i++
        out += escape()
        continue
      }
      out += c
      i++
    }
  }
  const value = (): TomlValue => {
    const c = peek()
    if (c === '"' || c === "'") return stringVal()
    if (c === '[') {
      i++
      const arr: TomlValue[] = []
      for (;;) {
        skipWsNl()
        if (peek() === ']') {
          i++
          return arr
        }
        arr.push(value())
        skipWsNl()
        if (peek() === ',') {
          i++
          continue
        }
        if (peek() === ']') {
          i++
          return arr
        }
        err('expected , or ] in array')
      }
    }
    if (c === '{') {
      i++
      const obj: Record<string, TomlValue> = {}
      skipWs()
      if (peek() === '}') {
        i++
        return obj
      }
      for (;;) {
        const k = key()
        if (peek() !== '=') err('expected = in inline table')
        i++
        skipWs()
        obj[k] = value()
        skipWs()
        if (peek() === ',') {
          i++
          skipWs()
          continue
        }
        if (peek() === '}') {
          i++
          return obj
        }
        err('expected , or } in inline table')
      }
    }
    const m = /^(true|false|[+-]?(?:inf|nan)|[+-]?[0-9][0-9_]*(?:\.[0-9_]+)?(?:[eE][+-]?[0-9]+)?|\d{4}-\d{2}-\d{2}[T ]?[0-9:.Z+-]*)/.exec(text.slice(i, i + 64))
    if (!m) return err('expected a value')
    i += m[0].length
    if (m[0] === 'true') return true
    if (m[0] === 'false') return false
    if (/^\d{4}-/.test(m[0])) return m[0]
    return Number(m[0].replace(/_/g, ''))
  }

  while (i < n) {
    skipWsNl()
    if (i >= n) break
    if (peek() === '[') {
      const start = i
      const hline = line
      const array = peek(1) === '['
      i += array ? 2 : 1
      const name = key()
      if (array) {
        if (!text.startsWith(']]', i)) err('expected ]] after table name')
        i += 2
      } else {
        if (peek() !== ']') err('expected ] after table name')
        i++
      }
      skipWs()
      skipComment()
      if (i < n && text[i] !== '\n' && text[i] !== '\r') err('text after table header')
      while (i < n && text[i] !== '\n') i++
      if (i < n) adv()
      cur.end = start
      cur = { name, array, start, bodyStart: i, end: n, line: hline, entries: [] }
      tables.push(cur)
      continue
    }
    const start = i
    const kline = line
    const k = key()
    if (peek() !== '=') err(`expected = after ${k}`)
    i++
    skipWs()
    const v = value()
    const end = i
    if (cur.entries.some((e) => e.key === k)) throw new TomlError(`duplicate key ${k}`, kline)
    cur.entries.push({ key: k, value: v, start, end, line: kline })
    skipWs()
    skipComment()
    if (i < n && text[i] !== '\n' && text[i] !== '\r') err('expected end of line')
  }
  return tables
}

// ---------- template model ----------

export interface TplCheck {
  kind: string
  message?: string
  [k: string]: TomlValue | undefined
}

export interface TplStep {
  index: number
  id: string
  title: string
  summary?: string
  agent?: string
  /** undefined: after the step before; []: starts at once. */
  needs?: string[]
  each?: { from: string; max?: number }
  gate: boolean
  decide?: { options?: string; question?: string }
  fixes: string[]
  attempts?: number
  prompt?: string
  outputs: string[]
  checks: TplCheck[]
  optional: boolean
  when?: string
  table: Table
}

export interface Template {
  id?: string
  name?: string
  description?: string
  inputs: { key: string; label?: string; kind?: string }[]
  steps: TplStep[]
}

const str = (v: TomlValue | undefined) => (typeof v === 'string' ? v : undefined)
const strArr = (v: TomlValue | undefined) => (Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : undefined)
const get = (t: Table, k: string) => t.entries.find((e) => e.key === k)?.value

export function readTemplate(text: string): { template: Template; tables: Table[] } {
  const tables = parseToml(text)
  const root = tables[0]
  const steps: TplStep[] = []
  const inputs: Template['inputs'] = []
  for (const t of tables) {
    if (t.array && t.name === 'inputs') inputs.push({ key: str(get(t, 'key')) ?? '', label: str(get(t, 'label')), kind: str(get(t, 'kind')) })
    if (!(t.array && t.name === 'steps')) continue
    const each = get(t, 'each')
    const decide = get(t, 'decide')
    const checks = get(t, 'checks')
    steps.push({
      index: steps.length,
      id: str(get(t, 'id')) ?? '',
      title: str(get(t, 'title')) ?? '',
      summary: str(get(t, 'summary')),
      agent: str(get(t, 'agent')),
      needs: strArr(get(t, 'needs')),
      each: each && typeof each === 'object' && !Array.isArray(each) ? { from: str(each.from) ?? '', max: typeof each.max === 'number' ? each.max : undefined } : undefined,
      gate: get(t, 'gate') === true,
      decide: decide && typeof decide === 'object' && !Array.isArray(decide) ? { options: str(decide.options), question: str(decide.question) } : undefined,
      fixes: strArr(get(t, 'fixes')) ?? [],
      attempts: typeof get(t, 'attempts') === 'number' ? (get(t, 'attempts') as number) : undefined,
      prompt: str(get(t, 'prompt')),
      outputs: strArr(get(t, 'outputs')) ?? [],
      checks: Array.isArray(checks) ? (checks.filter((c) => c && typeof c === 'object' && !Array.isArray(c)) as TplCheck[]) : [],
      optional: get(t, 'optional') === true,
      when: str(get(t, 'when')),
      table: t,
    })
  }
  return {
    template: { id: str(get(root, 'id')), name: str(get(root, 'name')), description: str(get(root, 'description')), inputs, steps },
    tables,
  }
}

/** The steps a step waits for, with the implicit "step before" filled in. */
export function effectiveNeeds(steps: TplStep[], s: TplStep): string[] {
  if (s.needs) return s.needs
  return s.index > 0 ? [steps[s.index - 1].id] : []
}

export interface Problem {
  line?: number
  message: string
  step?: string
}

/** Structural checks the editor shows next to TOML errors. Messages are built by the caller's t(). */
export function lintTemplate(tpl: Template, msg: { dup: (id: string) => string; unknown: (step: string, need: string) => string; cycle: (ids: string) => string }): Problem[] {
  const out: Problem[] = []
  const ids = new Map<string, TplStep>()
  for (const s of tpl.steps) {
    if (ids.has(s.id)) out.push({ line: s.table.line, message: msg.dup(s.id), step: s.id })
    ids.set(s.id, s)
  }
  for (const s of tpl.steps) {
    for (const nd of [...(s.needs ?? []), ...s.fixes]) {
      const base = nd.split(':')[0]
      if (!ids.has(base)) {
        const e = s.table.entries.find((x) => x.key === 'needs' || x.key === 'fixes')
        out.push({ line: e?.line ?? s.table.line, message: msg.unknown(s.id, nd), step: s.id })
      }
    }
  }
  // cycle detection over effective needs
  const state = new Map<string, number>()
  const stack: string[] = []
  let cycle: string[] | null = null
  const visit = (id: string) => {
    if (cycle) return
    state.set(id, 1)
    stack.push(id)
    const s = ids.get(id)
    if (s)
      for (const nd of effectiveNeeds(tpl.steps, s)) {
        if (!ids.has(nd)) continue
        if (state.get(nd) === 1) {
          cycle = [...stack.slice(stack.indexOf(nd)), nd]
          return
        }
        if (!state.get(nd)) visit(nd)
      }
    stack.pop()
    state.set(id, 2)
  }
  for (const s of tpl.steps) if (!state.get(s.id)) visit(s.id)
  if (cycle) out.push({ message: msg.cycle((cycle as string[]).join(' → ')), line: ids.get((cycle as string[])[0])?.table.line })
  return out
}

// ---------- writing ----------

export function tomlString(s: string): string {
  if (s.includes('\n')) {
    const body = s.replace(/\\/g, '\\\\').replace(/"""/g, '""\\"')
    return `"""\n${body}${body.endsWith('\n') ? '' : '\n'}"""`
  }
  return `"${s.replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\t/g, '\\t')}"`
}

export function tomlValue(v: TomlValue): string {
  if (typeof v === 'string') return tomlString(v)
  if (typeof v === 'number' || typeof v === 'boolean') return String(v)
  if (Array.isArray(v)) return `[${v.map(tomlValue).join(', ')}]`
  return `{ ${Object.entries(v)
    .map(([k, x]) => `${/^[A-Za-z0-9_-]+$/.test(k) ? k : tomlString(k)} = ${tomlValue(x)}`)
    .join(', ')} }`
}

const ORDER = ['id', 'title', 'summary', 'agent', 'skill', 'optional', 'when', 'needs', 'each', 'timeout_min', 'prompt', 'outputs', 'checks', 'gate', 'decide', 'fixes', 'attempts']

function lineStart(text: string, at: number) {
  const p = text.lastIndexOf('\n', at - 1)
  return p + 1
}
function lineEndIncl(text: string, at: number) {
  const p = text.indexOf('\n', at)
  return p < 0 ? text.length : p + 1
}

/** Set (or with `undefined`, remove) one key of a step. Returns the new text. */
export function setStepKey(text: string, step: TplStep, key: string, value: TomlValue | undefined): string {
  const t = step.table
  const e = t.entries.find((x) => x.key === key)
  if (e) {
    if (value === undefined) {
      const s = lineStart(text, e.start)
      const after = text.slice(e.end, lineEndIncl(text, e.end))
      // drop the whole line(s) only when nothing but a comment follows the value
      const end = /^\s*(#.*)?\n?$/.test(after) ? lineEndIncl(text, e.end) : e.end
      return text.slice(0, s) + text.slice(end)
    }
    return text.slice(0, e.start) + `${key} = ${tomlValue(value)}` + text.slice(e.end)
  }
  if (value === undefined) return text
  // insert after the latest existing key that comes before this one in the usual order
  const rank = ORDER.indexOf(key)
  let anchor: Entry | undefined
  for (const x of t.entries) {
    const r = ORDER.indexOf(x.key)
    if (r !== -1 && r < (rank === -1 ? ORDER.length : rank) && (!anchor || x.end > anchor.end)) anchor = x
  }
  const at = anchor ? lineEndIncl(text, anchor.end) : t.bodyStart
  const prefix = at > 0 && text[at - 1] !== '\n' ? '\n' : ''
  return text.slice(0, at) + `${prefix}${key} = ${tomlValue(value)}\n` + text.slice(at)
}

export function appendStep(text: string, id: string, title: string, needs?: string[]): string {
  const trimmed = text.replace(/\s*$/, '')
  const lines = ['', '[[steps]]', `id = ${tomlString(id)}`, `title = ${tomlString(title)}`]
  if (needs) lines.push(`needs = ${tomlValue(needs)}`)
  lines.push('prompt = """', '', '"""')
  return `${trimmed}\n${lines.join('\n')}\n`
}

/** Remove a step's block and every reference to it. */
export function removeStep(text: string, step: TplStep, all: TplStep[]): string {
  // patch references from the bottom up so earlier offsets stay valid
  const refs = all.filter((s) => s.id !== step.id && ((s.needs ?? []).includes(step.id) || s.fixes.some((f) => f.split(':')[0] === step.id))).sort((a, b) => b.table.start - a.table.start)
  let out = text
  // the block itself first if it is below all refs, else after; simplest: compute in one pass by order
  const jobs: { at: number; run: (s: string) => string }[] = refs.map((s) => ({
    at: s.table.start,
    run: (src: string) => {
      let r = src
      if (s.fixes.some((f) => f.split(':')[0] === step.id)) {
        const next = s.fixes.filter((f) => f.split(':')[0] !== step.id)
        r = setStepKey(r, s, 'fixes', next.length ? next : undefined)
      }
      if ((s.needs ?? []).includes(step.id)) {
        // re-read the table after a fixes edit (it may sit before needs)
        const fresh = readTemplate(r).template.steps.find((x) => x.id === s.id)!
        r = setStepKey(r, fresh, 'needs', (s.needs ?? []).filter((x) => x !== step.id))
      }
      return r
    },
  }))
  jobs.push({
    at: step.table.start,
    run: (src: string) => {
      const fresh = readTemplate(src).template.steps.find((x) => x.id === step.id && x.index === step.index) ?? readTemplate(src).template.steps.find((x) => x.id === step.id)!
      // also swallow blank lines just before the header
      let s = fresh.table.start
      while (s > 0 && /\s/.test(src[s - 1])) s--
      return src.slice(0, s) + (fresh.table.end >= src.length ? '\n' : '\n\n') + src.slice(fresh.table.end).replace(/^\s*/, '')
    },
  })
  jobs.sort((a, b) => b.at - a.at)
  for (const j of jobs) out = j.run(out)
  return out.replace(/\n{3,}/g, '\n\n')
}

/** Rename a step id everywhere it is referenced. */
export function renameStep(text: string, step: TplStep, all: TplStep[], next: string): string {
  const jobs = all
    .filter((s) => s === step || (s.needs ?? []).includes(step.id) || s.fixes.some((f) => f.split(':')[0] === step.id))
    .sort((a, b) => b.table.start - a.table.start)
  let out = text
  for (const s of jobs) {
    const fresh = () => readTemplate(out).template.steps[s.index]
    if (s.fixes.some((f) => f.split(':')[0] === step.id)) out = setStepKey(out, fresh(), 'fixes', s.fixes.map((f) => (f.split(':')[0] === step.id ? f.replace(step.id, next) : f)))
    if ((s.needs ?? []).includes(step.id)) out = setStepKey(out, fresh(), 'needs', (s.needs ?? []).map((x) => (x === step.id ? next : x)))
    if (s === step) out = setStepKey(out, fresh(), 'id', next)
  }
  return out
}
