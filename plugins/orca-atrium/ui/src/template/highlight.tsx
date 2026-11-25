// A small TOML highlighter that renders one <div> per line (so lines can be marked).
import type { VNode } from 'preact'

const VALUE = /("""|"(?:[^"\\]|\\.)*"|'[^']*'|#.*|\b(?:true|false)\b|-?\d+(?:\.\d+)?)/g

function tokens(line: string, key: { n: number }, setMl: (d: string | null) => void): VNode[] {
  const out: VNode[] = []
  const push = (cls: string | null, s: string) => s && out.push(cls ? <span key={key.n++} class={cls}>{s}</span> : <span key={key.n++}>{s}</span>)
  const trimmed = line.trimStart()
  const lead = line.slice(0, line.length - trimmed.length)
  if (trimmed.startsWith('#')) return [<span key={key.n++} class="tk-com">{line}</span>]
  const header = trimmed.match(/^(\[\[?)([^\]]*)(\]\]?)(.*)$/)
  if (header) {
    push(null, lead)
    push('tk-pun', header[1])
    push('tk-tab', header[2])
    push('tk-pun', header[3])
    push('tk-com', header[4])
    return out
  }
  let rest = line
  const kv = line.match(/^(\s*)([A-Za-z0-9_.-]+|"[^"]*")(\s*=\s*)(.*)$/)
  if (kv) {
    push(null, kv[1])
    push('tk-key', kv[2])
    push('tk-pun', kv[3])
    rest = kv[4]
  }
  let last = 0
  for (const m of rest.matchAll(VALUE)) {
    const i = m.index ?? 0
    push(null, rest.slice(last, i))
    const tok = m[0]
    if (tok === '"""') {
      // multi-line string: the rest of the line belongs to it
      push('tk-str', rest.slice(i))
      setMl('"""')
      return out
    }
    push(tok.startsWith('#') ? 'tk-com' : tok[0] === '"' || tok[0] === "'" ? 'tk-str' : /^(true|false)$/.test(tok) ? 'tk-kw' : 'tk-num', tok)
    last = i + tok.length
  }
  push(null, rest.slice(last))
  return out
}

export function highlight(text: string, errorLines: Set<number>): VNode[] {
  const key = { n: 0 }
  let ml: string | null = null
  return text.split('\n').map((line, idx) => {
    let body: VNode[]
    if (ml) {
      const end = line.indexOf(ml)
      if (end < 0) body = [<span key={key.n++} class="tk-str">{line}</span>]
      else {
        ml = null
        body = [<span key={key.n++} class="tk-str">{line.slice(0, end + 3)}</span>, ...tokens(line.slice(end + 3), key, (d) => (ml = d))]
      }
    } else body = tokens(line, key, (d) => (ml = d))
    return (
      <div key={idx} class={errorLines.has(idx + 1) ? 'ln ln-err' : 'ln'}>
        {body.length ? body : '​'}
      </div>
    )
  })
}
