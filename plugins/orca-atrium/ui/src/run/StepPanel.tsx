// The right drawer: what the selected step did, what you can do about it.
import { useEffect, useState } from 'preact/hooks'
import type { AttemptView, RunView, StepView } from '../../../worker/src/protocol'
import { request } from '../orca'
import { clock, duration, outcomeLabel, stepStatusLabel, t } from '../i18n'
import { Button, Chip, Select, TextArea, act, stepTone, type Tone } from '../design/components'
import { Icon } from '../design/icons'
import { Markdown } from '../design/markdown'
import { cx } from '../util'

const kindIcon = { agent: 'sparkle', gate: 'diamond', each: 'grid' } as const

export function StepPanel({ run, step, onSelect }: { run: RunView; step: StepView | null; onSelect: (id: string | null) => void }) {
  if (!step) return <Overview run={run} onSelect={onSelect} />
  return <StepDetail run={run} step={step} onSelect={onSelect} />
}

function Head({ children, title, sub }: { children?: preact.ComponentChildren; title: string; sub: preact.ComponentChildren }) {
  return (
    <header class="sp-head">
      <div class="col grow" style={{ gap: '4px' }}>
        <h2 class="sp-title">{title}</h2>
        <div class="sp-sub">{sub}</div>
      </div>
      {children}
    </header>
  )
}

function Overview({ run, onSelect }: { run: RunView; onSelect: (id: string | null) => void }) {
  const title = (id: string) => run.steps.find((s) => s.id === id)?.title ?? id
  return (
    <aside class="sp" aria-label={t('runOverview')}>
      <Head title={t('runOverview')} sub={<span class="muted">{t('selectStep')}</span>} />
      <div class="sp-body">
        <section class="sp-sec">
          <h3 class="sp-h">{t('inputs')}</h3>
          <dl class="sp-kv">
            {Object.entries(run.inputs).map(([k, v]) => (
              <div key={k} class="sp-kv-row">
                <dt>{k}</dt>
                <dd>{v}</dd>
              </div>
            ))}
          </dl>
        </section>
        <section class="sp-sec">
          <h3 class="sp-h">{t('decisions')}</h3>
          {run.decisions.length === 0 ? (
            <p class="muted">{t('noDecisions')}</p>
          ) : (
            <ol class="sp-log">
              {[...run.decisions].reverse().map((d, i) => (
                <li key={i}>
                  <span class="faint num">{clock(d.at)}</span>
                  <div>
                    <b>{title(d.step)}</b>
                    <div class="muted">{d.sendBackTo ? t('sentBack', { step: title(d.sendBackTo) }) : t('chosen', { choice: d.choice })}</div>
                    {d.note && <div class="sp-note">{t('noteLabel', { note: d.note })}</div>}
                  </div>
                </li>
              ))}
            </ol>
          )}
        </section>
        <section class="sp-sec">
          <h3 class="sp-h">{t('experience')}</h3>
          <p class="muted sp-hint">{t('experienceHint')}</p>
          {run.experience.length === 0 ? (
            <p class="muted">{t('noExperience')}</p>
          ) : (
            <ul class="sp-exp">
              {run.experience.map((e, i) => (
                <li key={i}>{e}</li>
              ))}
            </ul>
          )}
        </section>
        <Button size="sm" variant="quiet" onClick={() => onSelect(run.currentStep ?? null)} style={{ display: run.currentStep ? undefined : 'none' }}>
          {t('nowAt', { step: title(run.currentStep ?? '') })}
        </Button>
      </div>
    </aside>
  )
}

function StepDetail({ run, step, onSelect }: { run: RunView; step: StepView; onSelect: (id: string | null) => void }) {
  const [attemptIdx, setAttemptIdx] = useState(step.attempts.length - 1)
  // follow the newest attempt when the step or its attempts change
  useEffect(() => setAttemptIdx(step.attempts.length - 1), [step.id, step.attempts.length])
  const at: AttemptView | undefined = step.attempts[Math.min(attemptIdx, step.attempts.length - 1)]
  const tone = stepTone(step.status)
  const decisions = run.decisions.filter((d) => d.step === step.id)
  const byId = (id: string) => run.steps.find((s) => s.id === id)
  const dirOpen = run.state !== 'done' && run.state !== 'cancelled'

  return (
    <aside class="sp" aria-label={step.title}>
      <Head
        title={step.title}
        sub={
          <>
            <Chip tone={tone}>{stepStatusLabel(step.status)}</Chip>
            <span class="sp-kind">
              <Icon name={kindIcon[step.kind]} size={12} />
              {t(`stepKind_${step.kind}` as 'stepKind_agent')}
            </span>
            {step.attempts.length > 0 && <span class="muted num">{t('attemptN', { n: step.attempts.length })}</span>}
          </>
        }
      >
        <Button size="sm" variant="quiet" icon="x" aria-label={t('close')} onClick={() => onSelect(null)} />
      </Head>
      <div class="sp-body">
        {step.status === 'waiting' && <Decision run={run} step={step} />}

        {step.needs.length > 0 && (
          <div class="sp-needs">
            <span class="muted">{t('after')}</span>
            {step.needs.map((n) => (
              <button key={n} type="button" class="sp-link" onClick={() => onSelect(n)}>
                <i class={cx('sp-dot', `tone-${stepTone(byId(n)?.status ?? 'pending')}`)} />
                {byId(n)?.title ?? n}
              </button>
            ))}
          </div>
        )}

        {step.kind === 'each' && step.items && (
          <section class="sp-sec">
            <h3 class="sp-h">{t('items', { done: step.items.filter((i) => i.status === 'passed').length, total: step.items.length })}</h3>
            <ul class="sp-items">
              {step.items.map((i) => (
                <li key={i.id}>
                  <i class={cx('sp-dot', `tone-${stepTone(i.status)}`)} />
                  <span class="grow ellipsis">{i.label}</span>
                  <span class="muted">{stepStatusLabel(i.status)}</span>
                  {(i.status === 'passed' || i.status === 'failed') && dirOpen && (
                    <Button size="sm" variant="quiet" icon="refresh" aria-label={t('retryItem', { item: i.label })} title={t('retryItem', { item: i.label })} onClick={() => act(() => request('run.retry', { dir: run.dir, step: step.id, item: i.id }), t('retried', { step: i.label }))} />
                  )}
                </li>
              ))}
            </ul>
          </section>
        )}

        {step.attempts.length > 0 ? (
          <>
            {step.attempts.length > 1 && (
              <div class="sp-attempts" role="tablist" aria-label={t('attempts')}>
                {step.attempts.map((a, i) => (
                  <button key={a.n} type="button" role="tab" aria-selected={i === attemptIdx} class={cx('sp-att', i === attemptIdx && 'is-on')} onClick={() => setAttemptIdx(i)}>
                    <i class={cx('sp-dot', `tone-${attemptTone(a)}`)} />
                    {t('attemptN', { n: a.n })}
                  </button>
                ))}
              </div>
            )}
            {at && (
              <>
                <div class="sp-att-meta muted">
                  {at.outcome && <Chip tone={attemptTone(at)}>{outcomeLabel(at.outcome)}</Chip>}
                  <span class="num">{clock(at.startedAt)}</span>
                  <span>·</span>
                  <span class="num">{duration((at.endedAt ?? Date.now()) - at.startedAt)}</span>
                  {at.agent && <span class="faint">· {at.agent}</span>}
                </div>
                <section class="sp-sec">
                  <h3 class="sp-h">{t('report')}</h3>
                  {at.report ? <Markdown text={at.report} /> : <p class="muted">{t('noReport')}</p>}
                </section>
                <section class="sp-sec">
                  <h3 class="sp-h">{t('checks')}</h3>
                  {at.checks.length === 0 ? <p class="muted">{t('noChecks')}</p> : <ul class="sp-checks">{at.checks.map((c, i) => <Check key={i} c={c} />)}</ul>}
                </section>
              </>
            )}
          </>
        ) : (
          <p class="muted">{t('attemptsEmpty')}</p>
        )}

        {decisions.length > 0 && (
          <section class="sp-sec">
            <h3 class="sp-h">{t('decisions')}</h3>
            <ol class="sp-log">
              {decisions.map((d, i) => (
                <li key={i}>
                  <span class="faint num">{clock(d.at)}</span>
                  <div>
                    <div>{d.sendBackTo ? t('sentBack', { step: byId(d.sendBackTo)?.title ?? d.sendBackTo }) : t('chosen', { choice: d.choice })}</div>
                    {d.note && <div class="sp-note">{t('noteLabel', { note: d.note })}</div>}
                  </div>
                </li>
              ))}
            </ol>
          </section>
        )}

        {step.prompt && (
          <details class="sp-prompt">
            <summary>{t('prompt')}</summary>
            <pre class="md-pre">{step.prompt}</pre>
          </details>
        )}
      </div>
      {(step.terminalId || (dirOpen && step.status !== 'pending' && step.status !== 'waiting')) && (
        <footer class="sp-foot">
          {step.terminalId && (
            <Button icon="terminal" onClick={() => act(() => request('agent.focus', { terminalId: step.terminalId! }))}>
              {t('openAgent')}
            </Button>
          )}
          <span class="grow" />
          {dirOpen && step.status !== 'pending' && step.status !== 'waiting' && step.kind !== 'each' && (
            <Button icon="refresh" onClick={() => act(() => request('run.retry', { dir: run.dir, step: step.id }), t('retried', { step: step.title }))}>
              {t('retryStep')}
            </Button>
          )}
        </footer>
      )}
    </aside>
  )
}

function attemptTone(a: AttemptView): Tone {
  return a.outcome === 'passed' ? 'ok' : a.outcome === 'failed' ? 'bad' : a.outcome === 'sent_back' || a.outcome === 'stalled' ? 'wait' : 'run'
}

function Check({ c }: { c: AttemptView['checks'][number] }) {
  const [open, setOpen] = useState(false)
  const tone: Tone = c.ok === true ? 'ok' : c.ok === false ? 'bad' : 'idle'
  return (
    <li class={cx('chk', `tone-${tone}`)}>
      <span class="chk-mark">{c.ok === true ? <Icon name="check" size={12} /> : c.ok === false ? <Icon name="x" size={12} /> : <span class="chk-na" />}</span>
      <div class="col grow" style={{ gap: '2px' }}>
        <span>{c.label}</span>
        {c.detail && <span class="muted chk-detail">{c.detail}</span>}
        {c.ok === null && <span class="faint chk-detail">{t('checkPending')}</span>}
        {c.log && (
          <>
            <button type="button" class="sp-link chk-log-btn" onClick={() => setOpen(!open)} aria-expanded={open}>
              {open ? t('hideLog') : t('showLog')}
            </button>
            {open && <pre class="md-pre">{c.log}</pre>}
          </>
        )}
      </div>
    </li>
  )
}

function Decision({ run, step }: { run: RunView; step: StepView }) {
  const [note, setNote] = useState('')
  const [target, setTarget] = useState(step.sendBack?.[0] ?? '')
  const [back, setBack] = useState(false)
  const [busy, setBusy] = useState<string | null>(null)
  const go = async (key: string, choice: string, sendBackTo?: string, ok?: string) => {
    setBusy(key)
    await act(() => request('run.decide', { dir: run.dir, step: step.id, choice, sendBackTo, note: note.trim() || undefined }), ok)
    setBusy(null)
  }
  const targets = (step.sendBack ?? []).map((id) => ({ value: id, label: run.steps.find((s) => s.id === id)?.title ?? id }))
  return (
    <section class="sp-decide" aria-label={t('acting')}>
      <div class="sp-decide-head">
        <Icon name="diamond" size={14} />
        <span>{t('acting')}</span>
      </div>
      <p class="sp-decide-text">{t('gateWaiting')}</p>
      <TextArea value={note} onInput={setNote} rows={2} placeholder={back ? t('sendBackNote') : t('decisionNote')} aria-label={t('decisionNote')} />
      {!back ? (
        <div class="row wrap">
          <Button variant="primary" icon="check" loading={busy === 'approve'} disabled={!!busy} onClick={() => go('approve', 'approve', undefined, t('approved'))}>
            {t('approve')}
          </Button>
          {targets.length > 0 && (
            <Button icon="sendBack" disabled={!!busy} onClick={() => setBack(true)}>
              {t('sendBack')}…
            </Button>
          )}
        </div>
      ) : (
        <div class="col" style={{ gap: '8px' }}>
          <label class="row">
            <span class="muted nowrap">{t('sendBackTo')}</span>
            <Select value={target} onChange={setTarget} options={targets} label={t('sendBackTo')} />
          </label>
          <div class="row">
            <Button variant="primary" icon="sendBack" loading={busy === 'back'} disabled={!!busy || !target} onClick={() => go('back', 'send_back', target, t('sentBack', { step: targets.find((x) => x.value === target)?.label ?? target }))}>
              {t('sendBackConfirm')}
            </Button>
            <Button variant="ghost" onClick={() => setBack(false)}>
              {t('cancel')}
            </Button>
          </div>
        </div>
      )}
    </section>
  )
}
