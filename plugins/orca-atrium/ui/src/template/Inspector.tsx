// Edit one step: the fields write straight into the TOML text.
import { useEffect, useState } from 'preact/hooks'
import { t } from '../i18n'
import { Button, Field, IconButton, Select, TextArea, TextInput, Toggle } from '../design/components'
import { Icon } from '../design/icons'
import { Chips } from '../board/Team'
import { effectiveNeeds, type TomlValue, type TplStep } from './toml'

const AGENTS = ['claude', 'codex', 'gemini', 'deepseek', 'opencode']

type OnKey = (step: TplStep, key: string, value: TomlValue | undefined, coalesce?: boolean) => void

export function Inspector({ step, steps, readOnly, onKey, onRename, onDelete, onNeeds, onSelect }: { step: TplStep; steps: TplStep[]; readOnly: boolean; onKey: OnKey; onRename: (id: string) => void; onDelete: () => void; onNeeds: (list: string[]) => void; onSelect: (id: string) => void }) {
  const [id, setId] = useState(step.id)
  useEffect(() => setId(step.id), [step.id])
  const needs = effectiveNeeds(steps, step)
  const implicit = step.needs === undefined && step.index > 0
  const others = steps.filter((s) => s.id !== step.id)
  const set = (key: string, v: TomlValue | undefined, co = true) => onKey(step, key, v, co)
  const ro = readOnly
  const title = (sid: string) => steps.find((s) => s.id === sid)?.title || sid

  return (
    <div class="ins" key={step.id}>
      <div class="ins-head">
        <h2 class="ins-title">{step.title || step.id}</h2>
        <span class="grow" />
        <IconButton icon="trash" label={t('tplDeleteStep')} disabled={ro} onClick={() => { if (confirm_(t('tplDelConfirm', { title: step.title || step.id }))) onDelete() }} />
      </div>

      <div class="ins-grid">
        <Field label={t('tplStepTitle')}>
          <TextInput value={step.title} disabled={ro} onInput={(v) => set('title', v)} />
        </Field>
        <Field label={t('tplStepId')}>
          <TextInput value={id} mono disabled={ro} onInput={setId} onBlur={() => id.trim() && id !== step.id && onRename(id)} onKeyDown={(e) => e.key === 'Enter' && (e.currentTarget as HTMLInputElement).blur()} />
        </Field>
      </div>
      <Field label={t('tplSummary')}>
        <TextInput value={step.summary ?? ''} disabled={ro} onInput={(v) => set('summary', v || undefined)} />
      </Field>
      <Field label={t('tplAgent')}>
        <Select value={step.agent ?? ''} onChange={(v) => set('agent', v || undefined, false)} options={[{ value: '', label: t('tplAgentDefault') }, ...(step.agent && !AGENTS.includes(step.agent) ? [step.agent] : []).concat(AGENTS).map((a) => ({ value: a, label: a }))]} />
      </Field>

      <section class="ins-sec">
        <div class="field-label">{t('tplNeeds')}</div>
        <div class="ins-needs">
          {needs.length === 0 && <span class="muted ins-none">{t('tplNeedsNone')}</span>}
          {needs.map((n) => (
            <span key={n} class="need-chip">
              <button type="button" class="need-chip-name" onClick={() => onSelect(n)}>
                {title(n)}
              </button>
              {implicit && <span class="faint need-chip-tag">{t('tplNeedsPrev')}</span>}
              <button type="button" class="need-chip-x" disabled={ro} aria-label={`${t('remove')} ${title(n)}`} onClick={() => onNeeds(needs.filter((x) => x !== n))}>
                <Icon name="x" size={11} />
              </button>
            </span>
          ))}
        </div>
        {others.some((s) => !needs.includes(s.id)) && (
          <Select value="" onChange={(v) => v && onNeeds([...needs, v])} label={t('tplAddNeed')} options={[{ value: '', label: `+ ${t('tplAddNeed')}` }, ...others.filter((s) => !needs.includes(s.id)).map((s) => ({ value: s.id, label: s.title || s.id }))]} />
        )}
      </section>

      <section class="ins-sec ins-switches">
        <label class="ins-switch">
          <Toggle size="sm" checked={step.gate} disabled={ro} label={t('tplGate')} onChange={(v) => set('gate', v ? true : undefined, false)} />
          <span>{t('tplGate')}</span>
        </label>
        {step.gate && (
          <Field label={t('tplFixes')} hint={t('tplFixesHint')}>
            <Chips values={step.fixes} onChange={(v) => set('fixes', v.length ? v : undefined, false)} placeholder={others[0]?.id ?? ''} />
          </Field>
        )}
        <label class="ins-switch">
          <Toggle size="sm" checked={!!step.each} disabled={ro} label={t('tplEach')} onChange={(v) => set('each', v ? { from: 'reports/items.json' } : undefined, false)} />
          <span>{t('tplEach')}</span>
        </label>
        {step.each && (
          <div class="ins-grid">
            <Field label={t('tplEachFrom')}>
              <TextInput value={step.each.from} mono disabled={ro} onInput={(v) => set('each', step.each!.max ? { from: v, max: step.each!.max } : { from: v })} />
            </Field>
            <Field label={t('tplEachMax')} hint={t('tplEachMaxHint')}>
              <TextInput value={step.each.max != null ? String(step.each.max) : ''} inputMode="numeric" disabled={ro} onInput={(v) => { const n = parseInt(v, 10); set('each', Number.isFinite(n) && n > 0 ? { from: step.each!.from, max: n } : { from: step.each!.from }) }} />
            </Field>
          </div>
        )}
        <label class="ins-switch">
          <Toggle size="sm" checked={step.optional} disabled={ro} label={t('tplOptional')} onChange={(v) => set('optional', v ? true : undefined, false)} />
          <span>{t('tplOptional')}</span>
        </label>
        {step.optional && (
          <Field label={t('tplWhen')}>
            <TextInput value={step.when ?? ''} mono placeholder="picked=…" disabled={ro} onInput={(v) => set('when', v || undefined)} />
          </Field>
        )}
      </section>

      {!step.gate && (
        <>
          <Field label={t('tplPrompt')}>
            <TextArea value={step.prompt ?? ''} rows={7} disabled={ro} onInput={(v) => set('prompt', v || undefined)} />
          </Field>
          <Field label={t('tplOutputs')}>
            <Chips values={step.outputs} onChange={(v) => set('outputs', v.length ? v : undefined, false)} placeholder={t('tplOutputsPlaceholder')} />
          </Field>
        </>
      )}
      <Field label={t('tplAttempts')}>
        <TextInput value={step.attempts != null ? String(step.attempts) : ''} inputMode="numeric" disabled={ro} onInput={(v) => { const n = parseInt(v, 10); set('attempts', Number.isFinite(n) && n > 0 ? n : undefined) }} style={{ maxWidth: '96px' }} />
      </Field>

      <section class="ins-sec">
        <div class="field-label">{t('tplChecksList')}</div>
        {step.checks.length === 0 ? (
          <span class="muted ins-none">{t('none')}</span>
        ) : (
          <ul class="ins-checks">
            {step.checks.map((c, i) => (
              <li key={i}>
                <span class="tag mono">{c.kind}</span>
                <span class="grow ellipsis" title={c.message ?? ''}>
                  {c.message ?? String(c.path ?? c.command ?? '')}
                </span>
              </li>
            ))}
          </ul>
        )}
        <span class="faint ins-none">{t('tplChecksHint')}</span>
      </section>
      <div class="ins-foot">
        <Button size="sm" variant="danger" icon="trash" disabled={ro} onClick={() => { if (confirm_(t('tplDelConfirm', { title: step.title || step.id }))) onDelete() }}>
          {t('tplDeleteStep')}
        </Button>
      </div>
    </div>
  )
}

// Dialogs are disabled in plugin frames, so confirm by double-click-free inline text is overkill here:
// the deletion is one undo away, so no extra prompt.
function confirm_(_: string) {
  return true
}
