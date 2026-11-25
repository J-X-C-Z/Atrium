// The workflow run view (custom editor "run").
import { useState } from 'preact/hooks'
import type { RunState, RunView as Run } from '../../../worker/src/protocol'
import { request } from '../orca'
import { useLink, useResource, useTick } from '../data'
import { clock, duration, runStateLabel, stepStatusLabel, t } from '../i18n'
import { Button, Chip, EmptyState, ErrorState, Popover, SegmentedBar, Skeleton, act, stepTone, type Tone } from '../design/components'
import { Icon } from '../design/icons'
import { Dag } from './Dag'
import { StepPanel } from './StepPanel'
import './run.css'

export function RunView() {
  const link = useLink()
  if (link.status === 'loading')
    return (
      <div class="rn-pad">
        <Skeleton lines={5} height={16} />
      </div>
    )
  if (link.status === 'error')
    return (
      <div class="rn-pad">
        <EmptyState icon="alert" title={t('linkError')} hint={link.error} />
      </div>
    )
  if (link.link.kind !== 'run')
    return (
      <div class="rn-pad">
        <EmptyState icon="alert" title={t('runLinkWrong')} />
      </div>
    )
  return <RunFor dir={link.link.dir} />
}

const stateTone = (s: RunState): Tone => (s === 'waiting' ? 'wait' : s === 'running' ? 'run' : s === 'done' ? 'ok' : s === 'failed' ? 'bad' : 'idle')

function RunFor({ dir }: { dir: string }) {
  useTick(30000)
  const [res, reload] = useResource<Run>(() => request('run.get', { dir }) as Promise<Run>, [dir], { 'run.changed': (p) => p.dir === dir, 'agent.changed': () => true })
  const [picked, setPicked] = useState<string | null | undefined>(undefined)

  if (res.status === 'loading')
    return (
      <div class="rn-pad">
        <Skeleton lines={5} height={16} />
      </div>
    )
  if (res.status === 'error')
    return (
      <div class="rn-pad">
        <ErrorState error={res.error} onRetry={reload} />
      </div>
    )
  const run = res.data
  // Until you pick something, show the step that needs you (else the running one).
  const auto = run.steps.find((s) => s.status === 'waiting') ?? run.steps.find((s) => s.status === 'running' || s.status === 'checking') ?? null
  const sel = picked === undefined ? auto?.id ?? null : picked
  const step = run.steps.find((s) => s.id === sel) ?? null

  return (
    <div class="rn">
      <Header run={run} refreshing={res.refreshing} />
      <div class="rn-body">
        <div class="rn-graph">
          <Dag run={run} selected={sel} onSelect={setPicked} />
          <Legend />
        </div>
        <StepPanel run={run} step={step} onSelect={setPicked} />
      </div>
    </div>
  )
}

function Header({ run, refreshing }: { run: Run; refreshing: boolean }) {
  const [confirm, setConfirm] = useState(false)
  const [busy, setBusy] = useState<string | null>(null)
  const live = run.state === 'running' || run.state === 'waiting' || run.state === 'paused'
  const tone = stateTone(run.state)
  const go = async (key: string, fn: () => Promise<unknown>, ok: string) => {
    setBusy(key)
    await act(fn, ok)
    setBusy(null)
  }
  const segs = run.steps.map((s) => ({ tone: stepTone(s.status), title: `${s.title} · ${stepStatusLabel(s.status)}`, current: s.id === run.currentStep && live }))
  const inputs = Object.entries(run.inputs)
  const end = run.state === 'done' || run.state === 'failed' || run.state === 'cancelled'
  void end
  return (
    <header class="rn-head">
      <div class="rn-head-row">
        <span class="rn-mark" aria-hidden="true">
          <Icon name="route" size={16} />
        </span>
        <div class="rn-titles">
          <h1 class="rn-name ellipsis">{run.templateName}</h1>
          <div class="rn-sub">
            <Chip tone={tone}>{runStateLabel(run.state)}</Chip>
            <span class="muted num">{t('stepProgress', { done: run.progress.done, total: run.progress.total })}</span>
            <span class="faint">·</span>
            <span class="muted num">{t('elapsed', { t: duration(Date.now() - run.startedAt) })}</span>
            <span class="faint">·</span>
            <span class="faint num">{clock(run.startedAt)}</span>
            {refreshing && <span class="bd-sync-dot" aria-hidden="true" />}
          </div>
        </div>
        <span class="grow" />
        <div class="rn-actions">
          {(run.state === 'running' || run.state === 'waiting') && (
            <Button icon="pause" loading={busy === 'pause'} onClick={() => go('pause', () => request('run.pause', { dir: run.dir }), t('runPaused'))}>
              {t('pause')}
            </Button>
          )}
          {run.state === 'paused' && (
            <Button variant="primary" icon="play" loading={busy === 'resume'} onClick={() => go('resume', () => request('run.resume', { dir: run.dir }), t('runResumed'))}>
              {t('resume')}
            </Button>
          )}
          {live && (
            <span class="rn-cancel">
              <Button variant="danger" icon="stop" onClick={() => setConfirm(true)} aria-haspopup="dialog" aria-expanded={confirm}>
                {t('cancelRun')}
              </Button>
              <Popover open={confirm} onClose={() => setConfirm(false)} align="end">
                <div class="col" style={{ gap: '10px' }}>
                  <p>{t('cancelRunConfirm')}</p>
                  <div class="row" style={{ justifyContent: 'flex-end' }}>
                    <Button size="sm" variant="ghost" onClick={() => setConfirm(false)}>
                      {t('keepRunning')}
                    </Button>
                    <Button size="sm" variant="danger" loading={busy === 'cancel'} onClick={async () => { await go('cancel', () => request('run.cancel', { dir: run.dir }), t('runCancelled')); setConfirm(false) }}>
                      {t('confirmCancel')}
                    </Button>
                  </div>
                </div>
              </Popover>
            </span>
          )}
        </div>
      </div>
      <div class="rn-strip">
        <div class="rn-seg">
          <SegmentedBar segments={segs} label={t('stepProgress', { done: run.progress.done, total: run.progress.total })} />
        </div>
        <ul class="rn-inputs" aria-label={t('inputs')}>
          {inputs.slice(0, 4).map(([k, v]) => (
            <li key={k} title={`${k}: ${v}`}>
              <span class="faint">{k}</span>
              <span class="ellipsis">{v}</span>
            </li>
          ))}
          {inputs.length > 4 && <li class="faint">+{inputs.length - 4}</li>}
        </ul>
      </div>
    </header>
  )
}

function Legend() {
  const items: { tone: Tone; label: string }[] = [
    { tone: 'ok', label: stepStatusLabel('passed') },
    { tone: 'run', label: stepStatusLabel('running') },
    { tone: 'wait', label: stepStatusLabel('waiting') },
    { tone: 'bad', label: stepStatusLabel('failed') },
    { tone: 'idle', label: stepStatusLabel('pending') },
  ]
  return (
    <ul class="rn-legend" aria-label={t('legend')}>
      {items.map((i) => (
        <li key={i.label} class={`tone-${i.tone}`}>
          <i />
          {i.label}
        </li>
      ))}
      <li class="rn-legend-back">
        <svg width="26" height="8" viewBox="0 0 26 8" aria-hidden="true">
          <path d="M1 4h22" stroke="currentColor" stroke-width="1.5" stroke-dasharray="3 3" fill="none" />
          <path d="M20 1l4 3-4 3" stroke="currentColor" stroke-width="1.5" fill="none" />
        </svg>
        {t('sendBackEdge')}
      </li>
    </ul>
  )
}
