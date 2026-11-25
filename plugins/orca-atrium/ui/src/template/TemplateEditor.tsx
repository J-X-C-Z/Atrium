// The visual template editor: step graph + inspector + TOML, all editing the same document.
import { useEffect, useMemo, useRef, useState } from 'preact/hooks'
import type { StepView, ValidationError } from '../../../worker/src/protocol'
import { request } from '../orca'
import { t } from '../i18n'
import { Button, Chip, EmptyState, FullscreenButton, IconButton, Skeleton, toast } from '../design/components'
import { Icon } from '../design/icons'
import { cx } from '../util'
import { layoutSteps, type Box } from '../run/layout'
import { appendStep, effectiveNeeds, lintTemplate, readTemplate, removeStep, setStepKey, TomlError, type Template, type TplStep } from './toml'
import { useDoc } from './useDoc'
import { Inspector } from './Inspector'
import { Source } from './Source'
import './template.css'

interface Problem {
  line?: number
  message: string
  step?: string
}

export function TemplateEditor() {
  const doc = useDoc()
  const [sel, setSel] = useState<string | null>(null)
  const [past, setPast] = useState<string[]>([])
  const [future, setFuture] = useState<string[]>([])
  const lastEdit = useRef({ at: 0, key: '' })

  const text = doc.text
  const parsed = useMemo(() => {
    try {
      return { ...readTemplate(text), error: null as null | { message: string; line?: number } }
    } catch (e) {
      return { template: null as Template | null, tables: [], error: { message: (e as Error).message, line: e instanceof TomlError ? e.line : undefined } }
    }
  }, [text])
  const tpl = parsed.template

  // Problems: local structure checks, plus what the worker's validator says (debounced).
  const [remote, setRemote] = useState<ValidationError[]>([])
  useEffect(() => {
    if (doc.status !== 'ready') return
    const id = window.setTimeout(() => {
      request('template.validate', { text })
        .then((r) => setRemote(r.errors))
        .catch(() => setRemote([]))
    }, 400)
    return () => window.clearTimeout(id)
  }, [text, doc.status])
  const problems = useMemo<Problem[]>(() => {
    const out: Problem[] = []
    if (parsed.error) out.push({ line: parsed.error.line, message: t('tplParseError', { error: parsed.error.message }) })
    if (tpl) out.push(...lintTemplate(tpl, { dup: (id) => t('tplDupId', { id }), unknown: (s, n) => t('tplUnknownNeed', { step: s, need: n }), cycle: (s) => t('tplCycle', { steps: s }) }))
    for (const e of remote) if (!out.some((p) => p.line === e.line && (p.message === e.message || !!parsed.error))) out.push({ line: e.line, message: e.message })
    return out
  }, [parsed, tpl, remote])

  const steps = tpl?.steps ?? []
  const step = steps.find((s) => s.id === sel) ?? null
  // Start on the first step so the inspector is never an empty column.
  const started = useRef(false)
  useEffect(() => {
    if (sel && !steps.some((s) => s.id === sel)) setSel(null)
    if (!started.current && steps.length) {
      started.current = true
      setSel(steps.find((s) => s.each)?.id ?? steps[0].id)
    }
  }, [text])

  // ---- editing with undo ----
  const commit = (next: string, key = '') => {
    if (next === text || doc.readOnly) return
    const now = Date.now()
    if (!(key && lastEdit.current.key === key && now - lastEdit.current.at < 900)) setPast((p) => [...p.slice(-80), text])
    lastEdit.current = { at: now, key }
    setFuture([])
    doc.change(next)
  }
  const undo = () => {
    if (!past.length) return
    const prev = past[past.length - 1]
    setPast(past.slice(0, -1))
    setFuture([text, ...future])
    lastEdit.current = { at: 0, key: '' }
    doc.change(prev)
  }
  const redo = () => {
    if (!future.length) return
    setPast([...past, text])
    doc.change(future[0])
    setFuture(future.slice(1))
    lastEdit.current = { at: 0, key: '' }
  }
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey
      if (!mod) return
      const inField = (e.target as HTMLElement)?.tagName === 'TEXTAREA'
      if (inField) return // the browser's own undo handles typing in a field
      if (e.key.toLowerCase() === 'z') {
        e.preventDefault()
        e.shiftKey ? redo() : undo()
      } else if (e.key.toLowerCase() === 'y') {
        e.preventDefault()
        redo()
      }
    }
    window.addEventListener('keydown', key)
    return () => window.removeEventListener('keydown', key)
  })
  useEffect(() => {
    if (doc.outside) toast(t('tplConflict'))
  }, [doc.outside])

  const stepEdit = (s: TplStep, key: string, value: Parameters<typeof setStepKey>[3], coalesce = false) => commit(setStepKey(text, s, key, value), coalesce ? `${s.id}.${key}` : '')
  const addStep = () => {
    if (!tpl) return
    let n = steps.length + 1
    while (steps.some((s) => s.id === `step-${n}`)) n++
    const id = `step-${n}`
    commit(appendStep(text, id, t('tplNewStepTitle')))
    setSel(id)
  }
  const addEdge = (from: string, to: string) => {
    if (!tpl || from === to) return
    const target = steps.find((s) => s.id === to)
    if (!target) return
    const cur = effectiveNeeds(steps, target)
    if (cur.includes(from)) return
    if (reaches(steps, from, to)) return toast(t('tplCycle', { steps: `${to} → … → ${from}` }), 'bad')
    commit(setStepKey(text, target, 'needs', [...cur, from]))
  }
  const removeEdge = (from: string, to: string) => {
    const target = steps.find((s) => s.id === to)
    if (!target) return
    commit(setStepKey(text, target, 'needs', effectiveNeeds(steps, target).filter((x) => x !== from)))
  }

  if (doc.status === 'loading')
    return (
      <div class="tp-pad">
        <Skeleton lines={5} height={16} />
      </div>
    )
  if (doc.status === 'error')
    return (
      <div class="tp-pad">
        <EmptyState icon="alert" title={t('tplOpenFailed')} hint={doc.error} />
      </div>
    )

  const errLines = new Set(problems.map((p) => p.line).filter((x): x is number => !!x))
  const nProblems = problems.length
  return (
    <div class="tp">
      <header class="tp-bar">
        <span class="tp-mark" aria-hidden="true">
          <Icon name="route" size={15} />
        </span>
        <div class="tp-titles">
          <h1 class="tp-name ellipsis">{tpl?.name || tpl?.id || t('tplSteps')}</h1>
          <div class="tp-meta muted">
            {tpl && <span class="num">{t('stepsCount', { n: steps.length })}</span>}
            {tpl && tpl.inputs.length > 0 && (
              <>
                <span class="faint">·</span>
                <span class="num">{t('tplInputs', { n: tpl.inputs.length })}</span>
              </>
            )}
          </div>
        </div>
        <span class="grow" />
        {nProblems === 0 ? <Chip tone="ok" icon="check">{t('tplValid')}</Chip> : <Chip tone="bad" icon="alert">{t('tplErrors', { n: nProblems })}</Chip>}
        <div class="tp-tools">
          <IconButton icon="undo" label={t('tplUndo')} disabled={!past.length || doc.readOnly} onClick={undo} />
          <IconButton icon="redo" label={t('tplRedo')} disabled={!future.length || doc.readOnly} onClick={redo} />
          <FullscreenButton size="md" />
        </div>
        <Button variant="primary" icon="plus" disabled={doc.readOnly || !tpl} onClick={addStep}>
          {t('tplAddStep')}
        </Button>
      </header>
      {doc.readOnly && (
        <div class="tp-ro" role="status">
          <Icon name="alert" size={14} />
          {t('tplReadOnly')}
        </div>
      )}
      <div class="tp-body">
        <section class="tp-pane tp-graph" aria-label={t('tplSteps')}>
          {steps.length === 0 ? (
            <div class="tp-graph-empty">
              <EmptyState icon="route" title={t('tplNoSteps')} hint={t('tplNoStepsHint')} action={<Button variant="primary" icon="plus" disabled={doc.readOnly || !tpl} onClick={addStep}>{t('tplAddStep')}</Button>} />
            </div>
          ) : (
            <Graph steps={steps} selected={sel} onSelect={setSel} onConnect={addEdge} onDisconnect={removeEdge} readOnly={doc.readOnly} bad={new Set(problems.map((p) => p.step).filter((x): x is string => !!x))} />
          )}
          <p class="tp-hint muted">{t('tplConnectHint')}</p>
        </section>
        <section class="tp-pane tp-inspect" aria-label={t('tplFields')}>
          {step && tpl ? (
            <Inspector
              step={step}
              steps={steps}
              readOnly={doc.readOnly}
              onKey={stepEdit}
              onRename={(next) => {
                commit(renameSafe(text, step, steps, next))
                setSel(next)
              }}
              onDelete={() => {
                commit(removeStep(text, step, steps))
                setSel(null)
              }}
              onNeeds={(list) => commit(setStepKey(text, step, 'needs', list))}
              onSelect={setSel}
            />
          ) : (
            <div class="tp-inspect-empty">
              <EmptyState compact icon="pencil" title={t('tplSelectStep')} hint={tpl?.description} />
            </div>
          )}
        </section>
        <section class="tp-pane tp-src" aria-label={t('tplSource')}>
          <Source text={text} readOnly={doc.readOnly} onChange={(v) => commit(v, 'src')} problems={problems} errLines={errLines} />
        </section>
      </div>
    </div>
  )
}

import { renameStep } from './toml'
function renameSafe(text: string, step: TplStep, all: TplStep[], next: string) {
  const id = next.trim()
  if (!id || id === step.id) return text
  return renameStep(text, step, all, id)
}

/** Does `a` reach `b` following needs (a ← … ← b means adding b→a would loop)? */
function reaches(steps: TplStep[], from: string, to: string): boolean {
  // adding edge from → to is a cycle when `to` already (transitively) feeds `from`
  const seen = new Set<string>()
  const walk = (id: string): boolean => {
    if (id === to) return true
    if (seen.has(id)) return false
    seen.add(id)
    const s = steps.find((x) => x.id === id)
    return !!s && effectiveNeeds(steps, s).some(walk)
  }
  return walk(from)
}

// ---------- the graph ----------

function Graph({ steps, selected, onSelect, onConnect, onDisconnect, readOnly, bad }: { steps: TplStep[]; selected: string | null; onSelect: (id: string | null) => void; onConnect: (from: string, to: string) => void; onDisconnect: (from: string, to: string) => void; readOnly: boolean; bad: Set<string> }) {
  const views = useMemo<StepView[]>(
    () => steps.map((s) => ({ id: s.id, title: s.title || s.id, needs: effectiveNeeds(steps, s), kind: s.gate ? 'gate' : s.each ? 'each' : 'agent', status: 'pending', attempts: [] })),
    [steps],
  )
  const lay = useMemo(() => layoutSteps(views, 'v'), [views])
  const wrap = useRef<HTMLDivElement>(null)
  const canvas = useRef<HTMLDivElement>(null)
  const [scale, setScale] = useState(1)
  const [drag, setDrag] = useState<{ from: Box; x: number; y: number } | null>(null)
  const [edge, setEdge] = useState<{ from: string; to: string } | null>(null)

  useEffect(() => {
    const el = wrap.current
    if (!el) return
    const calc = () => setScale(Math.max(0.6, Math.min(1, (el.clientWidth - 8) / lay.width, (el.clientHeight - 56) / lay.height)))
    calc()
    const ro = new ResizeObserver(calc)
    ro.observe(el)
    return () => ro.disconnect()
  }, [lay.width])

  const toCanvas = (e: { clientX: number; clientY: number }) => {
    const r = canvas.current!.getBoundingClientRect()
    return { x: (e.clientX - r.left) / scale, y: (e.clientY - r.top) / scale }
  }
  const startConnect = (e: PointerEvent, from: Box) => {
    if (readOnly) return
    e.preventDefault()
    e.stopPropagation()
    const p = toCanvas(e)
    setDrag({ from, ...p })
    const move = (ev: PointerEvent) => setDrag((d) => (d ? { ...d, ...toCanvas(ev) } : d))
    const up = (ev: PointerEvent) => {
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', up)
      const q = toCanvas(ev)
      const hit = [...lay.boxes.values()].find((b) => q.x >= b.x - 6 && q.x <= b.x + b.w + 6 && q.y >= b.y - 6 && q.y <= b.y + b.h + 6)
      setDrag(null)
      if (hit && hit.step.id !== from.step.id) onConnect(from.step.id, hit.step.id)
    }
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', up)
  }

  const boxes = [...lay.boxes.values()]
  const edges: { from: Box; to: Box }[] = []
  for (const b of boxes) for (const n of b.step.needs) if (lay.boxes.has(n)) edges.push({ from: lay.boxes.get(n)!, to: b })
  const curve = (x1: number, y1: number, x2: number, y2: number) => {
    const dy = Math.max(20, Math.abs(y2 - y1) * 0.5)
    return `M${x1} ${y1} C${x1} ${y1 + dy} ${x2} ${y2 - dy} ${x2} ${y2}`
  }

  return (
    <div class="tp-scroll" ref={wrap} onClick={(e) => { if (e.target === e.currentTarget) { onSelect(null); setEdge(null) } }}>
      <div style={{ width: `${lay.width * scale}px`, height: `${lay.height * scale}px`, margin: 'auto' }}>
        <div class="dag-canvas" ref={canvas} style={{ width: `${lay.width}px`, height: `${lay.height}px`, transform: `scale(${scale})` }} onClick={(e) => { if (e.target === e.currentTarget) { onSelect(null); setEdge(null) } }}>
          <svg class="dag-svg" width={lay.width} height={lay.height}>
            <defs>
              <marker id="tp-ah" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto">
                <path d="M1 1 7 4 1 7" class="ah ah-n" />
              </marker>
              <marker id="tp-ah-sel" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto">
                <path d="M1 1 7 4 1 7" class="ah ah-wait" />
              </marker>
            </defs>
            {edges.map(({ from, to }) => {
              const on = edge?.from === from.step.id && edge.to === to.step.id
              const d = curve(from.outX, from.outY, to.inX, to.inY)
              return (
                <g key={`${from.step.id}>${to.step.id}`}>
                  <path class={cx('dag-edge', on && 'is-picked')} d={d} marker-end={`url(#${on ? 'tp-ah-sel' : 'tp-ah'})`} />
                  <path class="tp-edge-hit" d={d} onClick={(e) => { e.stopPropagation(); setEdge(on ? null : { from: from.step.id, to: to.step.id }) }} />
                </g>
              )
            })}
            {drag && <path class="dag-edge tp-edge-temp" d={curve(drag.from.outX, drag.from.outY, drag.x, drag.y)} />}
          </svg>
          {edge &&
            (() => {
              const a = lay.boxes.get(edge.from)
              const b = lay.boxes.get(edge.to)
              if (!a || !b) return null
              return (
                <button type="button" class="tp-edge-del" style={{ left: `${(a.outX + b.inX) / 2 - 12}px`, top: `${(a.outY + b.inY) / 2 - 12}px` }} aria-label={t('tplRemoveEdge')} title={t('tplRemoveEdge')} onClick={() => { onDisconnect(edge.from, edge.to); setEdge(null) }}>
                  <Icon name="x" size={12} />
                </button>
              )
            })()}
          {boxes.map((b) => {
            const s = steps.find((x) => x.id === b.step.id)!
            const gate = s.gate
            return (
              <div key={s.id} class="tnode-wrap" style={{ left: `${b.x}px`, top: `${b.y}px`, width: `${b.w}px`, height: `${b.h}px` }}>
                <button type="button" class={cx('dnode tnode', gate && 'gate tone-wait', selected === s.id && 'is-selected', bad.has(s.id) && 'is-bad', s.optional && 'is-optional')} style={{ width: '100%', height: '100%' }} onClick={() => onSelect(selected === s.id ? null : s.id)} aria-pressed={selected === s.id} aria-label={s.title || s.id}>
                  {gate ? (
                    <>
                      <svg class="gate-shape" width="64" height="64" viewBox="0 0 64 64" aria-hidden="true">
                        <path class="gate-halo" d="M32 2 62 32 32 62 2 32Z" />
                        <path class="gate-body" d="M32 8 56 32 32 56 8 32Z" />
                      </svg>
                      <span class="gate-icon">
                        <Icon name="diamond" size={14} />
                      </span>
                      <span class="gate-label">
                        <span class="dnode-title ellipsis">{s.title || s.id}</span>
                        <span class="dnode-sub mono">{s.id}</span>
                      </span>
                    </>
                  ) : (
                    <>
                      <span class="dnode-top">
                        <span class="dnode-glyph tone-neutral">
                          <Icon name={s.each ? 'grid' : 'sparkle'} size={12} />
                        </span>
                        <span class="dnode-title ellipsis">{s.title || s.id}</span>
                      </span>
                      <span class="dnode-sub row">
                        <span class="mono ellipsis">{s.id}</span>
                        <span class="grow" />
                        {s.agent && <span class="tnode-agent">{s.agent}</span>}
                        {s.checks.length > 0 && <span class="tnode-checks" title={t('tplChecks', { n: s.checks.length })}><Icon name="check" size={10} />{s.checks.length}</span>}
                      </span>
                    </>
                  )}
                </button>
                {!readOnly && <span class="tp-port" style={{ left: `${b.outX - b.x - 6}px`, top: `${b.outY - b.y - 6}px` }} onPointerDown={(e) => startConnect(e as unknown as PointerEvent, b)} title={t('tplDrag')} role="presentation" />}
              </div>
            )
          })}
        </div>
      </div>
    </div>
  )
}
