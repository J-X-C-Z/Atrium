// Milestones as a horizontal track: a marker per milestone, phases as small bars.
import type { ProjectView } from '../../../worker/src/protocol'
import { t } from '../i18n'
import { EmptyState, ProgressBar } from '../design/components'
import { cx } from '../util'

export function Milestones({ p }: { p: ProjectView }) {
  const stats = p.milestones.map((m) => {
    const done = m.phases.reduce((a, x) => a + x.done, 0)
    const total = m.phases.reduce((a, x) => a + x.total, 0)
    return { m, done, total, complete: total > 0 && done >= total }
  })
  const current = stats.findIndex((s) => !s.complete)
  return (
    <section class="bd-card bd-ms" aria-labelledby="h-ms">
      <div class="bd-card-head">
        <h2 id="h-ms" class="bd-card-title">
          {t('milestones')}
        </h2>
      </div>
      {stats.length === 0 ? (
        <EmptyState compact icon="flag" title={t('milestones')} hint={t('noMilestones')} />
      ) : (
        <ol class="ms-track">
          {stats.map((s, i) => {
            const state = s.complete ? 'done' : i === current ? 'now' : 'next'
            return (
              <li key={s.m.id} class={cx('ms', `ms-${state}`)} style={{ flexGrow: Math.max(1, s.m.phases.length) }}>
                <div class="ms-rail">
                  <span class="ms-marker" aria-hidden="true">
                    {state === 'done' && <svg width="10" height="10" viewBox="0 0 16 16"><path d="M3.2 8.4 6.4 11.4 12.8 4.8" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" /></svg>}
                  </span>
                  <span class="ms-line" />
                </div>
                <div class="ms-title">{s.m.title}</div>
                <div class="ms-count num muted">{t('milestoneProgress', { done: s.done, total: s.total })}</div>
                <div class="ms-phases">
                  {s.m.phases.map((ph) => (
                    <div key={ph.id} class="ms-phase" title={`${ph.title} ${ph.done}/${ph.total}`}>
                      <ProgressBar thin value={ph.done} total={ph.total} tone={ph.done >= ph.total ? 'ok' : 'run'} label={ph.title} />
                      <span class="ellipsis">{ph.title}</span>
                    </div>
                  ))}
                </div>
              </li>
            )
          })}
        </ol>
      )}
    </section>
  )
}
