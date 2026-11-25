// A small, safe Markdown renderer: builds VNodes, never HTML strings.
// Handles headings, paragraphs, lists, task lists, quotes, fences, pipe tables,
// inline code, bold, italic, strikethrough and links (opened through orca.shell).
import type { ComponentChildren, VNode } from 'preact'

function openLink(e: MouseEvent, href: string) {
  e.preventDefault()
  if (!/^(https?:|mailto:)/i.test(href)) return
  const shell = (window.orca as unknown as { shell?: { openExternal(u: string): unknown } })?.shell
  shell?.openExternal(href)
}

function inline(text: string, key = 0): ComponentChildren[] {
  const out: ComponentChildren[] = []
  const re = /(`+)([^`]+?)\1|\*\*([^*]+)\*\*|__([^_]+)__|\*([^*\s][^*]*?)\*|_([^_\s][^_]*?)_|~~([^~]+)~~|\[([^\]]+)\]\(([^)\s]+)\)/g
  let last = 0
  let m: RegExpExecArray | null
  let k = key
  while ((m = re.exec(text))) {
    if (m.index > last) out.push(text.slice(last, m.index))
    if (m[2] != null) out.push(<code key={k++}>{m[2]}</code>)
    else if (m[3] != null || m[4] != null) out.push(<strong key={k++}>{inline(m[3] ?? m[4], k * 10)}</strong>)
    else if (m[5] != null || m[6] != null) out.push(<em key={k++}>{inline(m[5] ?? m[6], k * 10)}</em>)
    else if (m[7] != null) out.push(<del key={k++}>{m[7]}</del>)
    else if (m[8] != null) {
      const href = m[9]
      const safe = /^(https?:|mailto:)/i.test(href)
      out.push(
        safe ? (
          <a key={k++} href={href} onClick={(e) => openLink(e as unknown as MouseEvent, href)} title={href}>
            {m[8]}
          </a>
        ) : (
          <span key={k++} class="md-ref" title={href}>
            {m[8]}
          </span>
        ),
      )
    }
    last = m.index + m[0].length
  }
  if (last < text.length) out.push(text.slice(last))
  return out
}

const splitRow = (line: string) =>
  line
    .trim()
    .replace(/^\||\|$/g, '')
    .split('|')
    .map((c) => c.trim())

export function Markdown({ text, class: cls }: { text: string; class?: string }) {
  const lines = text.replace(/\r\n?/g, '\n').split('\n')
  const blocks: VNode[] = []
  let i = 0
  let key = 0
  while (i < lines.length) {
    const line = lines[i]
    if (!line.trim()) {
      i++
      continue
    }
    const fence = line.match(/^\s*(```|~~~)\s*([\w-]*)/)
    if (fence) {
      const body: string[] = []
      i++
      while (i < lines.length && !lines[i].trim().startsWith(fence[1])) body.push(lines[i++])
      i++
      blocks.push(
        <pre key={key++} class="md-pre" data-lang={fence[2] || undefined}>
          <code>{body.join('\n')}</code>
        </pre>,
      )
      continue
    }
    const h = line.match(/^(#{1,6})\s+(.*)$/)
    if (h) {
      const level = Math.min(4, h[1].length + 1)
      const Tag = `h${level}` as 'h2'
      blocks.push(
        <Tag key={key++} class="md-h">
          {inline(h[2])}
        </Tag>,
      )
      i++
      continue
    }
    if (/^\s*([-*_])\s*\1\s*\1[\s\1]*$/.test(line)) {
      blocks.push(<hr key={key++} />)
      i++
      continue
    }
    if (/^\s*\|.*\|\s*$/.test(line) && i + 1 < lines.length && /^\s*\|?\s*:?-{2,}/.test(lines[i + 1])) {
      const head = splitRow(line)
      i += 2
      const rows: string[][] = []
      while (i < lines.length && /^\s*\|.*\|\s*$/.test(lines[i])) rows.push(splitRow(lines[i++]))
      blocks.push(
        <div key={key++} class="md-table-wrap">
          <table class="md-table">
            <thead>
              <tr>
                {head.map((c, j) => (
                  <th key={j}>{inline(c)}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {rows.map((r, ri) => (
                <tr key={ri}>
                  {r.map((c, j) => (
                    <td key={j}>{inline(c)}</td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>,
      )
      continue
    }
    if (/^\s*>/.test(line)) {
      const body: string[] = []
      while (i < lines.length && /^\s*>/.test(lines[i])) body.push(lines[i++].replace(/^\s*>\s?/, ''))
      blocks.push(
        <blockquote key={key++}>
          <Markdown text={body.join('\n')} />
        </blockquote>,
      )
      continue
    }
    const li = line.match(/^(\s*)([-*+]|\d+[.)])\s+(.*)$/)
    if (li) {
      const ordered = /\d/.test(li[2])
      const items: ComponentChildren[] = []
      while (i < lines.length) {
        const m = lines[i].match(/^(\s*)([-*+]|\d+[.)])\s+(.*)$/)
        if (!m) {
          if (lines[i].trim() && /^\s{2,}/.test(lines[i]) && items.length) {
            i++
            continue
          }
          break
        }
        const task = m[3].match(/^\[([ xX])\]\s+(.*)$/)
        items.push(
          <li key={items.length} class={task ? 'md-task' : undefined}>
            {task && <span class={`md-check ${task[1] !== ' ' ? 'is-done' : ''}`} aria-hidden="true" />}
            {inline(task ? task[2] : m[3])}
          </li>,
        )
        i++
      }
      blocks.push(ordered ? <ol key={key++}>{items}</ol> : <ul key={key++}>{items}</ul>)
      continue
    }
    const para: string[] = []
    while (i < lines.length && lines[i].trim() && !/^(#{1,6}\s|\s*```|\s*>|\s*([-*+]|\d+[.)])\s)/.test(lines[i])) para.push(lines[i++])
    if (!para.length) para.push(lines[i++])
    blocks.push(<p key={key++}>{inline(para.join(' '))}</p>)
  }
  return <div class={`md ${cls ?? ''}`}>{blocks}</div>
}
