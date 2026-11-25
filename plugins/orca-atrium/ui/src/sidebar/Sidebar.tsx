// The Atrium sidebar: what needs you, then projects and runs.
import { useEffect, useState } from 'preact/hooks'
import type { LiveAgent, NeedItem, ProjectSummary, RunSummary, TemplateSummary } from '../../../worker/src/protocol'
import { request } from '../orca'
import { useResource, useTick } from '../data'
import { ago, duration, lang, needKindLabel, t } from '../i18n'
import { Button, Chip, EmptyState, ErrorState, Field, FullscreenButton, Menu, ProgressBar, SegmentedBar, Select, Skeleton, TextArea, TextInput, Toggle, act, agentTone, toast, type Tone } from '../design/components'
import { Icon, type IconName } from '../design/icons'
import { basename, cx, roleTitle } from '../util'
import './sidebar.css'

type Overview = { projects: ProjectSummary[]; runs: RunSummary[]; needs: NeedItem[]; agents: LiveAgent[]; lang: 'zh' | 'en' }

const NEED_ICON: Record<NeedItem['kind'], IconName> = { gate: 'diamond', question: 'message', stalled: 'clock', blocked: 'alert', brief: 'file', quota: 'gauge', error: 'alert' }
const NEED_TONE: Record<NeedItem['kind'], Tone> = { gate: 'wait', question: 'run', stalled: 'wait', blocked: 'bad', brief: 'run', quota: 'bad', error: 'bad' }

export function Sidebar() {
  useTick(30000)
  const [res, reload] = useResource<Overview>(() => request('overview.get') as Promise<Overview>, [], ['overview.changed', 'needs.changed', 'project.changed', 'run.changed', 'agent.changed'])
  const [starting, setStarting] = useState(false)

  if (res.status === 'loading')
    return (
      <div class="sb">
        <Header needs={0} running={0} loading />
        <div class="sb-scroll">
          <Skeleton lines={4} height={14} />
          <Skeleton lines={3} height={14} />
        </div>
      </div>
    )
  if (res.status === 'error')
    return (
      <div class="sb">
        <Header needs={0} running={0} />
        <div class="sb-scroll">
          <ErrorState error={res.error} onRetry={reload} />
        </div>
      </div>
    )

  const o = res.data
  const running = o.runs.filter((r) => r.state === 'running' || r.state === 'waiting').length + o.agents.filter((a) => a.state === 'working').length
  const pick = lang() // re-render key when the language flips
  return (
    <div class="sb" data-lang={pick}>
      <Header needs={o.needs.length} running={o.agents.filter((a) => a.state === 'working').length} refreshing={res.refreshing} />
      <div class="sb-scroll">
        <NeedsSection o={o} />
        <ProjectsSection o={o} />
        <RunsSection o={o} onStart={() => setStarting(true)} />
        <span class="sr-only">{running}</span>
      </div>
      {starting && <StartRun onClose={() => setStarting(false)} />}
    </div>
  )
}

function Header({ needs, running, loading, refreshing }: { needs: number; running: number; loading?: boolean; refreshing?: boolean }) {
  return (
    <header class="sb-head">
      <span class="sb-mark" aria-hidden="true">
        <Icon name="layers" size={15} />
      </span>
      <h1 class="sb-title">Atrium</h1>
      <span class="grow" />
      {!loading && (
        <div class="row" style={{ gap: '6px' }} aria-live="polite">
          {needs > 0 && <Chip tone="wait" solid>{t('needsCount', { n: needs })}</Chip>}
          {running > 0 && <Chip tone="run">{t('runningCount', { n: running })}</Chip>}
          {needs === 0 && running === 0 && <span class="faint sb-quiet">{t('allClear')}</span>}
        </div>
      )}
      <span class={cx('sb-pulse', refreshing && 'is-on')} aria-hidden="true" />
      <FullscreenButton size="sm" />
    </header>
  )
}

// ---------- needs you ----------

function sourceLabel(n: NeedItem, o: Overview): string {
  if (n.source.kind === 'project') {
    const root = n.source.root
    const name = o.projects.find((p) => p.root === root)?.name ?? basename(root)
    return n.source.role ? `${name} · ${roleTitle(n.source.role)}` : name
  }
  const dir = n.source.dir
  return o.runs.find((r) => r.dir === dir)?.templateName ?? basename(dir)
}

function openSource(n: NeedItem) {
  if (n.source.kind === 'project') act(() => request('board.open', { root: n.source.kind === 'project' ? n.source.root : '' }))
  else act(() => request('run.open', { dir: n.source.kind === 'run' ? n.source.dir : '' }))
}

function NeedsSection({ o }: { o: Overview }) {
  return (
    <section class="sb-section" aria-labelledby="h-needs">
      <div class="sb-sec-head">
        <h2 id="h-needs" class="sb-sec-title">
          {t('needsYou')}
        </h2>
        {o.needs.length > 0 && <span class="sb-count tone-wait num">{o.needs.length}</span>}
      </div>
      {o.needs.length === 0 ? (
        <div class="sb-clear">
          <span class="sb-clear-mark">
            <Icon name="check" size={14} />
          </span>
          <div>
            <div class="sb-clear-title">{t('allClear')}</div>
            <p class="muted sb-clear-hint">{t('allClearHint')}</p>
          </div>
        </div>
      ) : (
        <ul class="sb-list">
          {o.needs.map((n) => (
            <li key={n.id}>
              <NeedCard n={n} source={sourceLabel(n, o)} />
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}

function NeedCard({ n, source }: { n: NeedItem; source: string }) {
  const [busy, setBusy] = useState<string | null>(null)
  const [back, setBack] = useState<{ step: string } | null>(null)
  const [note, setNote] = useState('')
  const [answer, setAnswer] = useState('')
  const tone = NEED_TONE[n.kind]
  const run = n.source.kind === 'run' ? n.source : null

  const go = async (key: string, fn: () => Promise<unknown>, ok?: string) => {
    setBusy(key)
    await act(fn, ok)
    setBusy(null)
  }
  const decide = (choice: string, sendBackTo?: string) => go(choice, () => request('run.decide', { dir: run!.dir, step: run!.step ?? '', choice, sendBackTo, note: note.trim() || undefined }), sendBackTo ? t('sentBack', { step: n.sendBackTargets?.find((x) => x.step === sendBackTo)?.label ?? sendBackTo }) : t('approved'))

  return (
    <article class={cx('need', `tone-${tone}`)} aria-label={n.title}>
      <div class="need-top">
        <span class="need-icon">
          <Icon name={NEED_ICON[n.kind]} size={14} />
        </span>
        <div class="need-main">
          <div class="need-kind">
            <span>{needKindLabel(n.kind)}</span>
            <span class="faint">·</span>
            <span class="faint">{ago(n.createdAt)}</span>
          </div>
          <button type="button" class="need-title" onClick={() => openSource(n)} title={t('open')}>
            {n.title}
          </button>
          <div class="need-src ellipsis">{source}</div>
          {n.detail && <p class="need-detail">{n.detail}</p>}
        </div>
      </div>

      {n.kind === 'gate' && run && !back && (
        <div class="need-actions">
          {(n.choices?.length ? n.choices : [{ id: 'approve', label: t('approve') }]).map((c, i) => (
            <Button key={c.id} size="sm" variant={i === 0 ? 'primary' : 'secondary'} icon={i === 0 ? 'check' : undefined} loading={busy === c.id} disabled={!!busy} onClick={() => decide(c.id)}>
              {c.label}
            </Button>
          ))}
          {!!n.sendBackTargets?.length && (
            <Menu
              label={t('sendBackTo')}
              heading={t('sendBackTo')}
              align="end"
              items={n.sendBackTargets.map((x) => ({ id: x.step, label: x.label }))}
              onSelect={(id) => setBack({ step: id })}
              trigger={({ toggle, open, id }) => (
                <Button size="sm" variant="secondary" icon="sendBack" iconRight="chevronDown" disabled={!!busy} onClick={toggle} aria-haspopup="menu" aria-expanded={open} aria-controls={id}>
                  {t('sendBack')}
                </Button>
              )}
            />
          )}
        </div>
      )}
      {n.kind === 'gate' && run && back && (
        <div class="need-form">
          <div class="need-form-title">
            <Icon name="sendBack" size={13} />
            {t('sendBackTitle', { step: n.sendBackTargets?.find((x) => x.step === back.step)?.label ?? back.step })}
          </div>
          <TextArea value={note} onInput={setNote} rows={3} placeholder={t('sendBackNote')} aria-label={t('sendBackNote')} autoFocus />
          <div class="row">
            <Button size="sm" variant="primary" loading={busy === 'send_back'} onClick={() => decide('send_back', back.step)}>
              {t('sendBackConfirm')}
            </Button>
            <Button size="sm" variant="ghost" onClick={() => setBack(null)}>
              {t('cancel')}
            </Button>
          </div>
        </div>
      )}

      {n.kind === 'question' && (
        <form
          class="need-form"
          onSubmit={(e) => {
            e.preventDefault()
            if (answer.trim()) go('reply', () => request('need.resolve', { id: n.id, answer: answer.trim() }), t('replied'))
          }}
        >
          <TextArea value={answer} onInput={setAnswer} rows={2} placeholder={t('replyPlaceholder')} aria-label={t('reply')} onKeyDown={(e) => { if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) (e.currentTarget.form as HTMLFormElement)?.requestSubmit() }} />
          <div class="row">
            <Button size="sm" variant="primary" icon="message" loading={busy === 'reply'} disabled={!answer.trim()} {...{ type: 'submit' }}>
              {t('reply')}
            </Button>
            {n.terminalId && (
              <Button size="sm" variant="ghost" icon="terminal" onClick={() => act(() => request('agent.focus', { terminalId: n.terminalId! }))}>
                {t('openAgent')}
              </Button>
            )}
          </div>
        </form>
      )}

      {(n.kind === 'stalled' || n.kind === 'blocked' || n.kind === 'quota' || n.kind === 'error') && (
        <div class="need-actions">
          {n.terminalId && n.kind === 'stalled' && (
            <Button size="sm" variant="primary" icon="nudge" loading={busy === 'nudge'} disabled={!!busy} onClick={() => go('nudge', () => request('agent.nudge', { terminalId: n.terminalId! }), t('nudged'))}>
              {t('nudge')}
            </Button>
          )}
          {n.terminalId && (
            <Button size="sm" variant="secondary" icon="terminal" onClick={() => act(() => request('agent.focus', { terminalId: n.terminalId! }))}>
              {t('openAgent')}
            </Button>
          )}
          <Button size="sm" variant="quiet" loading={busy === 'dismiss'} onClick={() => go('dismiss', () => request('need.resolve', { id: n.id }))}>
            {t('dismiss')}
          </Button>
        </div>
      )}

      {n.kind === 'brief' && (
        <div class="need-actions">
          <Button size="sm" variant="primary" icon="file" onClick={() => openSource(n)}>
            {t('openBoard')}
          </Button>
          <Button size="sm" variant="quiet" onClick={() => go('dismiss', () => request('need.resolve', { id: n.id }))}>
            {t('dismiss')}
          </Button>
        </div>
      )}
    </article>
  )
}

// ---------- projects ----------

function ProjectsSection({ o }: { o: Overview }) {
  const [busy, setBusy] = useState(false)
  const makeProject = async () => {
    setBusy(true)
    await act(async () => {
      const { root } = await request('project.init', {})
      await request('board.open', { root })
    })
    setBusy(false)
  }
  return (
    <section class="sb-section" aria-labelledby="h-projects">
      <div class="sb-sec-head">
        <h2 id="h-projects" class="sb-sec-title">
          {t('projects')}
        </h2>
        {o.projects.length > 0 && <span class="sb-count num">{o.projects.length}</span>}
        <span class="grow" />
        {o.projects.length > 0 && <Button size="sm" variant="quiet" icon="plus" loading={busy} onClick={makeProject} title={t('makeProject')} aria-label={t('makeProject')} />}
      </div>
      {o.projects.length === 0 ? (
        <EmptyState icon="users" title={t('noProjects')} hint={t('noProjectsHint')} action={<Button size="sm" variant="primary" icon="plus" loading={busy} onClick={makeProject}>{t('makeProject')}</Button>} />
      ) : (
        <ul class="sb-list">
          {o.projects.map((p) => (
            <li key={p.root}>
              <ProjectCard p={p} />
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}

function RoleDots({ roles }: { roles: ProjectSummary['roles'] }) {
  return (
    <ul class="dots" aria-label={t('rolesLive', { working: roles.filter((r) => r.live === 'working').length, total: roles.length })}>
      {roles.map((r) => (
        <li key={r.id} class={cx('dot', r.live && `dot-${r.live}`, `tone-${agentTone(r.live)}`)} title={`${roleTitle(r.id)} · ${r.live ? t(`st_${r.live}` as 'st_idle') : t('st_unbound')}`}>
          <span class="sr-only">{roleTitle(r.id)}</span>
        </li>
      ))}
    </ul>
  )
}

function ProjectCard({ p }: { p: ProjectSummary }) {
  const [on, setOn] = useState(p.autopilot)
  // keep in step with the worker when it changes elsewhere
  useEffect(() => setOn(p.autopilot), [p.autopilot])
  const working = p.roles.filter((r) => r.live === 'working').length
  const pct = p.progress.total ? Math.round((p.progress.done / p.progress.total) * 100) : 0
  return (
    <article class="pcard">
      <div class="pcard-top">
        <div class="grow">
          <button type="button" class="pcard-name ellipsis" onClick={() => act(() => request('board.open', { root: p.root }))}>
            {p.name}
          </button>
          <div class="pcard-path ellipsis faint mono" title={p.root}>
            {p.root}
          </div>
        </div>
        {p.needs > 0 && <Chip tone="wait" icon="alert">{t('projectNeeds', { n: p.needs })}</Chip>}
      </div>

      <div class="pcard-progress">
        <ProgressBar value={p.progress.done} total={p.progress.total} label={t('milestoneProgress', { done: p.progress.done, total: p.progress.total })} />
        <div class="row pcard-meta">
          <span class="num">{t('milestoneProgress', { done: p.progress.done, total: p.progress.total })}</span>
          <span class="grow" />
          <span class="num faint">{pct}%</span>
        </div>
      </div>

      <div class="pcard-roles">
        <RoleDots roles={p.roles} />
        <span class="muted pcard-roles-text">{t('rolesLive', { working, total: p.roles.length })}</span>
      </div>

      <div class="pcard-foot">
        <label class="pcard-auto" title={on ? t('autopilotOn') : t('autopilotOff')}>
          <Toggle
            size="sm"
            checked={on}
            label={t('autopilot')}
            onChange={async (v) => {
              setOn(v)
              const ok = await act(() => request('project.autopilot', { root: p.root, on: v }))
              if (!ok) setOn(!v)
              else toast(v ? t('autopilotOn') : t('autopilotOff'), 'neutral')
            }}
          />
          <span class={on ? '' : 'muted'}>{t('autopilot')}</span>
        </label>
        <span class="grow" />
        <Button size="sm" variant="secondary" iconRight="chevronRight" onClick={() => act(() => request('board.open', { root: p.root }))}>
          {t('openBoard')}
        </Button>
      </div>
    </article>
  )
}

// ---------- runs ----------

function RunsSection({ o, onStart }: { o: Overview; onStart: () => void }) {
  const runs = [...o.runs].sort((a, b) => rank(a) - rank(b) || b.startedAt - a.startedAt)
  return (
    <section class="sb-section" aria-labelledby="h-runs">
      <div class="sb-sec-head">
        <h2 id="h-runs" class="sb-sec-title">
          {t('runs')}
        </h2>
        {runs.length > 0 && <span class="sb-count num">{runs.length}</span>}
        <span class="grow" />
        {runs.length > 0 && <Button size="sm" variant="quiet" icon="plus" onClick={onStart} title={t('startWorkflow')} aria-label={t('startWorkflow')} />}
      </div>
      {runs.length === 0 ? (
        <EmptyState icon="route" title={t('noRuns')} hint={t('noRunsHint')} action={<Button size="sm" variant="primary" icon="play" onClick={onStart}>{t('startWorkflow')}</Button>} />
      ) : (
        <ul class="sb-list">
          {runs.map((r) => (
            <li key={r.dir}>
              <RunCard r={r} />
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}

const rank = (r: RunSummary) => (r.state === 'waiting' ? 0 : r.state === 'running' ? 1 : r.state === 'paused' ? 2 : 3)

function runTone(s: RunSummary['state']): Tone {
  return s === 'waiting' ? 'wait' : s === 'running' ? 'run' : s === 'done' ? 'ok' : s === 'failed' ? 'bad' : 'idle'
}

function RunCard({ r }: { r: RunSummary }) {
  const tone = runTone(r.state)
  const live = r.state === 'running' || r.state === 'waiting'
  const segs = Array.from({ length: r.progress.total }, (_, i): { tone: Tone; current?: boolean } => {
    if (i < r.progress.done) return { tone: 'ok' }
    if (i === r.progress.done && live) return { tone: r.state === 'waiting' ? 'wait' : 'run', current: true }
    return { tone: 'idle' }
  })
  return (
    <article class={cx('rcard', `tone-${tone}`, live && 'is-live')}>
      <div class="rcard-top">
        <button type="button" class="rcard-name ellipsis" onClick={() => act(() => request('run.open', { dir: r.dir }))}>
          {r.templateName}
        </button>
        <Chip tone={tone}>{t(`rs_${r.state}` as 'rs_running')}</Chip>
      </div>
      <SegmentedBar segments={segs} label={t('stepProgress', { done: r.progress.done, total: r.progress.total })} />
      <div class="rcard-meta">
        <span class="num">{t('stepProgress', { done: r.progress.done, total: r.progress.total })}</span>
        <span class="faint">·</span>
        <span class="muted num">{t('elapsed', { t: duration(Date.now() - r.startedAt) })}</span>
        <span class="grow" />
        <Button size="sm" variant="ghost" iconRight="chevronRight" onClick={() => act(() => request('run.open', { dir: r.dir }))}>
          {t('open')}
        </Button>
      </div>
    </article>
  )
}

// ---------- start a workflow ----------

function StartRun({ onClose }: { onClose: () => void }) {
  const [list] = useResource<TemplateSummary[]>(() => request('templates.list') as Promise<TemplateSummary[]>, [], [])
  const [pick, setPick] = useState<string>('')
  const [vals, setVals] = useState<Record<string, string>>({})
  const [busy, setBusy] = useState(false)
  const templates = list.status === 'ready' ? list.data.filter((x) => !x.error) : []
  const cur = templates.find((x) => x.id === pick) ?? templates[0]
  const missing = cur ? cur.inputs.some((i) => i.required && !(vals[i.id] ?? i.default ?? '').trim()) : true
  const start = async () => {
    if (!cur) return
    setBusy(true)
    const inputs: Record<string, string> = {}
    for (const i of cur.inputs) inputs[i.id] = vals[i.id] ?? i.default ?? ''
    const ok = await act(async () => {
      const { dir } = await request('run.start', { templateId: cur.id, inputs })
      await request('run.open', { dir })
    })
    setBusy(false)
    if (ok) onClose()
  }
  return (
    <div class="sheet-wrap" onKeyDown={(e) => e.key === 'Escape' && onClose()}>
      <button type="button" class="sheet-scrim" aria-label={t('close')} onClick={onClose} />
      <div class="sheet" role="dialog" aria-modal="true" aria-labelledby="sheet-title">
        <div class="row">
          <h2 id="sheet-title" class="sheet-title grow">
            {t('startWorkflow')}
          </h2>
          <Button size="sm" variant="quiet" icon="x" aria-label={t('close')} onClick={onClose} />
        </div>
        {list.status === 'loading' && <Skeleton lines={3} />}
        {list.status === 'error' && <ErrorState error={list.error} />}
        {cur && (
          <div class="col" style={{ gap: '12px' }}>
            <Field label={t('steps')}>
              <Select value={cur.id} onChange={(v) => { setPick(v); setVals({}) }} options={templates.map((x) => ({ value: x.id, label: `${x.name} · ${x.steps}` }))} label={t('steps')} />
            </Field>
            {cur.description && <p class="muted" style={{ fontSize: 'var(--fs-sm)' }}>{cur.description}</p>}
            {cur.inputs.map((i) => (
              <Field key={i.id} label={i.label + (i.required ? ' *' : '')}>
                {i.options?.length ? (
                  <Select value={vals[i.id] ?? i.default ?? i.options[0]} onChange={(v) => setVals({ ...vals, [i.id]: v })} options={i.options.map((o) => ({ value: o, label: o }))} label={i.label} />
                ) : i.kind === 'long' ? (
                  <TextArea value={vals[i.id] ?? i.default ?? ''} onInput={(v) => setVals({ ...vals, [i.id]: v })} rows={3} />
                ) : (
                  <TextInput value={vals[i.id] ?? i.default ?? ''} onInput={(v) => setVals({ ...vals, [i.id]: v })} />
                )}
              </Field>
            ))}
            <div class="row" style={{ justifyContent: 'flex-end' }}>
              <Button variant="ghost" onClick={onClose}>{t('cancel')}</Button>
              <Button variant="primary" icon="play" loading={busy} disabled={missing} onClick={start}>{t('startWorkflow')}</Button>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}
