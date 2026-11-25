// The TOML source: a textarea over a highlighted mirror, with line numbers and a problems list.
import { useRef } from 'preact/hooks'
import { t } from '../i18n'
import { Icon } from '../design/icons'
import { highlight } from './highlight'

export function Source({ text, readOnly, onChange, problems, errLines }: { text: string; readOnly: boolean; onChange: (v: string) => void; problems: { line?: number; message: string }[]; errLines: Set<number> }) {
  const ta = useRef<HTMLTextAreaElement>(null)
  const mirror = useRef<HTMLPreElement>(null)
  const gutter = useRef<HTMLDivElement>(null)
  const lines = text.split('\n').length
  const sync = () => {
    const el = ta.current
    if (!el) return
    const tr = `translate(${-el.scrollLeft}px, ${-el.scrollTop}px)`
    if (mirror.current) mirror.current.style.transform = tr
    if (gutter.current) gutter.current.style.transform = `translateY(${-el.scrollTop}px)`
  }
  const jump = (line: number) => {
    const el = ta.current
    if (!el) return
    const rows = text.split('\n')
    let pos = 0
    for (let i = 0; i < line - 1 && i < rows.length; i++) pos += rows[i].length + 1
    el.focus()
    el.setSelectionRange(pos, pos + (rows[line - 1]?.length ?? 0))
    el.scrollTop = Math.max(0, (line - 4) * 20)
    sync()
  }
  return (
    <>
      <div class="src-head">
        <span class="src-title">{t('tplSource')}</span>
        <span class="faint num">{lines}</span>
      </div>
      <div class="src-editor">
        <div class="src-gutter" aria-hidden="true">
          <div ref={gutter} class="src-gutter-in">
            {Array.from({ length: lines }, (_, i) => (
              <div key={i} class={errLines.has(i + 1) ? 'gn gn-err' : 'gn'}>
                {i + 1}
              </div>
            ))}
          </div>
        </div>
        <div class="src-text">
          <pre ref={mirror} class="src-mirror" aria-hidden="true">
            {highlight(text, errLines)}
          </pre>
          <textarea
            ref={ta}
            class="src-area"
            value={text}
            readOnly={readOnly}
            spellcheck={false}
            wrap="off"
            aria-label={t('tplSource')}
            onInput={(e) => onChange((e.target as HTMLTextAreaElement).value)}
            onScroll={sync}
            onKeyDown={(e) => {
              if (e.key === 'Tab' && !e.shiftKey && !readOnly) {
                e.preventDefault()
                const el = e.currentTarget
                const { selectionStart: a, selectionEnd: b } = el
                onChange(text.slice(0, a) + '  ' + text.slice(b))
                requestAnimationFrame(() => el.setSelectionRange(a + 2, a + 2))
              }
            }}
          />
        </div>
      </div>
      <div class="src-problems" role="status" aria-live="polite">
        {problems.length === 0 ? (
          <div class="src-ok">
            <Icon name="check" size={13} />
            {t('tplValid')}
          </div>
        ) : (
          <ul>
            {problems.map((p, i) => (
              <li key={i}>
                <button type="button" class="src-prob" disabled={!p.line} onClick={() => p.line && jump(p.line)}>
                  <Icon name="alert" size={13} class="src-prob-icon" />
                  <span class="grow">{p.message}</span>
                  {p.line && <span class="faint num nowrap">{t('tplLine', { n: p.line })}</span>}
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </>
  )
}
