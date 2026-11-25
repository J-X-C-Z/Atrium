// The step graph: nodes, dependency curves and dashed send-back curves.
import { useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks'
import type { Decision, RunView, StepStatus } from '../../../worker/src/protocol'
import { stepStatusLabel, t } from '../i18n'
import { IconButton, stepTone } from '../design/components'
import { Icon, type IconName } from '../design/icons'
import { cx } from '../util'
import { CELL, CELL_GAP, layoutSteps, type Box } from './layout'

const STATUS_ICON: Partial<Record<StepStatus, IconName>> = { passed: 'check', failed: 'x', sent_back: 'undo', waiting: 'pause', skipped: 'dot' }

function sentBackCount(d: Decision[], from: string, to?: string) {
  return d.filter((x) => x.step === from && x.sendBackTo && (!to || x.sendBackTo === to)).length
}

export function Dag({ run, selected, onSelect }: { run: RunView; selected: string | null; onSelect: (id: string | null) => void }) {
  const lay = useMemo(() => layoutSteps(run.steps), [run.steps])
  const wrap = useRef<HTMLDivElement>(null)
  const [fit, setFit] = useState(1)
  const [zoom, setZoom] = useState<number | null>(null)
  const scale = zoom ?? fit

  useLayoutEffect(() => {
    const el = wrap.current
    if (!el) return
    const calc = () => setFit(Math.max(0.62, Math.min(1, (el.clientWidth - 8) / lay.width)))
    calc()
    const ro = new ResizeObserver(calc)
    ro.observe(el)
    return () => ro.disconnect()
  }, [lay.width])

  const boxes = [...lay.boxes.values()]
  const edges: { from: Box; to: Box }[] = []
  for (const b of boxes) for (const n of b.step.needs) if (lay.boxes.has(n)) edges.push({ from: lay.boxes.get(n)!, to: b })
  const backs: { from: Box; to: Box; k: number; n: number }[] = []
  for (const b of boxes)
    (b.step.sendBack ?? []).forEach((id, k) => {
      const n = sentBackCount(run.decisions, b.step.id, id)
      // possible send-backs only show for the selected step; taken ones always
      if (lay.boxes.has(id) && (n > 0 || selected === b.step.id)) backs.push({ from: b, to: lay.boxes.get(id)!, k, n })
    })
  const top = Math.min(...boxes.map((b) => b.y), 0)

  return (
    <div class="dag">
      <div class="dag-tools" role="group" aria-label={t('zoomFit')}>
        <IconButton icon="minus" size="sm" label={t('zoomOut')} onClick={() => setZoom(Math.max(0.5, +(scale - 0.1).toFixed(2)))} />
        <button type="button" class="dag-zoom num" onClick={() => setZoom(null)} title={t('zoomFit')}>
          {Math.round(scale * 100)}%
        </button>
        <IconButton icon="plus" size="sm" label={t('zoomIn')} onClick={() => setZoom(Math.min(1.6, +(scale + 0.1).toFixed(2)))} />
      </div>
      <div class="dag-scroll" ref={wrap} onClick={(e) => e.target === e.currentTarget && onSelect(null)}>
        <div style={{ width: `${lay.width * scale}px`, height: `${lay.height * scale}px` }}>
          <div class="dag-canvas" style={{ width: `${lay.width}px`, height: `${lay.height}px`, transform: `scale(${scale})` }} onClick={(e) => e.target === e.currentTarget && onSelect(null)}>
            <svg class="dag-svg" width={lay.width} height={lay.height} aria-hidden="true">
              <defs>
                <marker id="ah-n" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto">
                  <path d="M1 1 7 4 1 7" class="ah ah-n" />
                </marker>
                <marker id="ah-ok" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto">
                  <path d="M1 1 7 4 1 7" class="ah ah-ok" />
                </marker>
                <marker id="ah-run" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto">
                  <path d="M1 1 7 4 1 7" class="ah ah-run" />
                </marker>
                <marker id="ah-wait" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto">
                  <path d="M1 1 7 4 1 7" class="ah ah-wait" />
                </marker>
              </defs>
              {edges.map(({ from, to }) => {
                const x1 = from.outX
                const y1 = from.outY
                const x2 = to.inX
                const y2 = to.inY
                const dx = Math.max(24, (x2 - x1) * 0.5)
                const done = from.step.status === 'passed' || from.step.status === 'skipped'
                const active = done && (to.step.status === 'running' || to.step.status === 'checking')
                const kind = active ? 'run' : done ? 'ok' : 'n'
                return <path key={`${from.step.id}>${to.step.id}`} class={cx('dag-edge', `e-${kind}`, active && 'is-flow')} d={`M${x1} ${y1} C${x1 + dx} ${y1} ${x2 - dx} ${y2} ${x2} ${y2}`} marker-end={`url(#ah-${kind})`} />
              })}
              {backs.map(({ from, to, k, n }) => {
                const gate = (b: Box) => b.step.kind === 'gate'
                const sx = from.x + from.w / 2
                const sy = from.y + (gate(from) ? 6 : 0)
                const tx = to.x + to.w / 2
                const ty = to.y + (gate(to) ? 6 : 0)
                const T = top - 26 - k * 14
                const taken = n > 0
                return (
                  <g key={`${from.step.id}<${to.step.id}`} class={cx('dag-back', taken && 'is-taken')}>
                    <path d={`M${sx} ${sy} C${sx} ${T} ${tx} ${T} ${tx} ${ty - 3}`} marker-end={taken ? 'url(#ah-wait)' : 'url(#ah-n)'} />
                    {taken && (
                      <text x={(sx + tx) / 2} y={(sy + 6 * T + ty) / 8 - 5} text-anchor="middle">
                        {t('sendBackEdge')}
                        {n > 1 ? ` ×${n}` : ''}
                      </text>
                    )}
                  </g>
                )
              })}
            </svg>
            {boxes.map((b) => (
              <Node key={b.step.id} b={b} selected={selected === b.step.id} onSelect={() => onSelect(selected === b.step.id ? null : b.step.id)} back={run.decisions.filter((d) => d.sendBackTo === b.step.id).length} />
            ))}
          </div>
        </div>
      </div>
    </div>
  )
}

function Node({ b, selected, onSelect, back }: { b: Box; selected: boolean; onSelect: () => void; back: number }) {
  const s = b.step
  const tone = stepTone(s.status)
  const live = s.status === 'running' || s.status === 'checking' || s.status === 'waiting'
  const last = s.attempts[s.attempts.length - 1]
  const checks = last?.checks ?? []
  const label = `${s.title}, ${stepStatusLabel(s.status)}`

  if (s.kind === 'gate') {
    return (
      <button type="button" class={cx('dnode gate', `tone-${tone}`, `st-${s.status}`, selected && 'is-selected', live && 'is-live')} style={{ left: `${b.x}px`, top: `${b.y}px`, width: `${b.w}px`, height: `${b.h}px` }} onClick={onSelect} aria-pressed={selected} aria-label={label}>
        <svg class="gate-shape" width="64" height="64" viewBox="0 0 64 64" aria-hidden="true">
          <path class="gate-halo" d="M32 2 62 32 32 62 2 32Z" />
          <path class="gate-body" d="M32 8 56 32 32 56 8 32Z" />
        </svg>
        <span class="gate-icon">
          <StatusGlyph s={s.status} size={16} fallback="diamond" />
        </span>
        <span class="gate-label">
          <span class="dnode-title ellipsis">{s.title}</span>
          <span class="dnode-sub">{stepStatusLabel(s.status)}</span>
        </span>
      </button>
    )
  }
  return (
    <button type="button" class={cx('dnode', `tone-${tone}`, `st-${s.status}`, selected && 'is-selected', live && 'is-live')} style={{ left: `${b.x}px`, top: `${b.y}px`, width: `${b.w}px`, height: `${b.h}px` }} onClick={onSelect} aria-pressed={selected} aria-label={label}>
      <span class="dnode-top">
        <span class="dnode-glyph">
          <StatusGlyph s={s.status} size={13} />
        </span>
        <span class="dnode-title ellipsis">{s.title}</span>
      </span>
      <span class="dnode-sub row">
        <span class="ellipsis">{stepStatusLabel(s.status)}</span>
        {s.attempts.length > 1 && <span class="dnode-try num">{t('attemptN', { n: s.attempts.length })}</span>}
        {back > 0 && s.attempts.length <= 1 && <span class="dnode-try num">{t('sendBackCount', { n: back })}</span>}
        {checks.length > 0 && (
          <span class="dnode-checks" title={t('checks')}>
            {checks.map((c, i) => (
              <i key={i} class={c.ok === true ? 'ck-ok' : c.ok === false ? 'ck-bad' : 'ck-na'} />
            ))}
          </span>
        )}
      </span>
      {s.kind === 'each' && s.items && (
        <span class="dnode-items" style={{ gridTemplateColumns: `repeat(${b.cols}, ${CELL}px)`, gap: `${CELL_GAP}px` }} role="img" aria-label={t('items', { done: s.items.filter((i) => i.status === 'passed').length, total: s.items.length })}>
          {s.items.map((i) => (
            <i key={i.id} class={cx('cell', `cell-${stepTone(i.status)}`, (i.status === 'running' || i.status === 'checking') && 'is-live')} title={`${i.label} · ${stepStatusLabel(i.status)}`} />
          ))}
        </span>
      )}
    </button>
  )
}

function StatusGlyph({ s, size, fallback }: { s: StepStatus; size: number; fallback?: IconName }) {
  if (s === 'running' || s === 'checking')
    return (
      <svg class="spin-ring" width={size} height={size} viewBox="0 0 16 16" aria-hidden="true">
        <circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" stroke-opacity="0.25" stroke-width="2" />
        <path d="M8 2a6 6 0 0 1 6 6" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
      </svg>
    )
  const icon = STATUS_ICON[s]
  if (icon) return <Icon name={icon} size={size} />
  return fallback ? <Icon name={fallback} size={size} /> : <span class="pending-ring" style={{ width: `${size - 2}px`, height: `${size - 2}px` }} />
}

