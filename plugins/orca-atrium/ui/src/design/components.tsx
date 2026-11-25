// Atrium's small component kit. Styling lives in components.css.
import type { ComponentChildren, JSX, RefObject } from 'preact'
import { useEffect, useLayoutEffect, useRef, useState, useCallback, useId } from 'preact/hooks'
import type { AgentState, StepStatus } from '../../../worker/src/protocol'
import { Icon, type IconName } from './icons'
import { agentStateLabel, t } from '../i18n'
import { signal } from './store'

const cx = (...parts: (string | false | null | undefined)[]) => parts.filter(Boolean).join(' ')

// ---------- state colours ----------

export type Tone = 'run' | 'wait' | 'ok' | 'bad' | 'idle' | 'neutral'

export function agentTone(s?: AgentState): Tone {
  switch (s) {
    case 'working':
    case 'starting':
      return 'run'
    case 'waiting':
    case 'stalled':
      return 'wait'
    case 'blocked':
      return 'bad'
    case 'done':
      return 'ok'
    default:
      return 'idle'
  }
}

export function stepTone(s: StepStatus): Tone {
  switch (s) {
    case 'running':
    case 'checking':
      return 'run'
    case 'waiting':
    case 'sent_back':
      return 'wait'
    case 'failed':
      return 'bad'
    case 'passed':
      return 'ok'
    default:
      return 'idle'
  }
}

export const toneVar = (tone: Tone) => (tone === 'neutral' ? 'var(--a-muted-fg)' : `var(--s-${tone})`)

// ---------- buttons ----------

type ButtonProps = {
  variant?: 'primary' | 'secondary' | 'ghost' | 'danger' | 'quiet'
  size?: 'sm' | 'md'
  icon?: IconName
  iconRight?: IconName
  loading?: boolean
  children?: ComponentChildren
} & Omit<JSX.HTMLAttributes<HTMLButtonElement>, 'size' | 'icon'>

export function Button({ variant = 'secondary', size = 'md', icon, iconRight, loading, children, class: cls, disabled, ...rest }: ButtonProps & { disabled?: boolean }) {
  return (
    <button
      type="button"
      class={cx('btn', `btn-${variant}`, `btn-${size}`, !children && 'btn-icon-only', loading && 'is-loading', cls as string)}
      disabled={disabled || loading}
      aria-busy={loading || undefined}
      {...rest}
    >
      {loading ? <Spinner size={size === 'sm' ? 12 : 14} /> : icon && <Icon name={icon} size={size === 'sm' ? 14 : 16} />}
      {children && <span class="btn-label">{children}</span>}
      {iconRight && <Icon name={iconRight} size={14} class="btn-icon-right" />}
    </button>
  )
}

export function IconButton({ icon, label, size = 'md', class: cls, ...rest }: { icon: IconName; label: string; size?: 'sm' | 'md' } & Omit<JSX.HTMLAttributes<HTMLButtonElement>, 'icon' | 'size' | 'label'> & { disabled?: boolean }) {
  return (
    <button type="button" class={cx('icon-btn', `icon-btn-${size}`, cls as string)} aria-label={label} title={label} {...rest}>
      <Icon name={icon} size={size === 'sm' ? 14 : 16} />
    </button>
  )
}

export function Spinner({ size = 14 }: { size?: number }) {
  return (
    <svg class="spinner" width={size} height={size} viewBox="0 0 16 16" aria-hidden="true">
      <circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" stroke-opacity="0.25" stroke-width="2" />
      <path d="M8 2a6 6 0 0 1 6 6" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
    </svg>
  )
}

// ---------- chips ----------

export function Chip({ tone = 'neutral', icon, children, class: cls, solid, title }: { tone?: Tone; icon?: IconName; children: ComponentChildren; class?: string; solid?: boolean; title?: string }) {
  return (
    <span class={cx('chip', `tone-${tone}`, solid && 'chip-solid', cls)} title={title}>
      {icon ? <Icon name={icon} size={12} /> : tone !== 'neutral' && <span class="chip-dot" />}
      <span class="chip-label">{children}</span>
    </span>
  )
}

export function Tag({ children, title, mono }: { children: ComponentChildren; title?: string; mono?: boolean }) {
  return (
    <span class={cx('tag', mono && 'mono')} title={title}>
      {children}
    </span>
  )
}

export function CountBadge({ n, tone = 'neutral' }: { n: number; tone?: Tone }) {
  return <span class={cx('count', `tone-${tone}`, 'num')}>{n}</span>
}

// ---------- toggle ----------

export function Toggle({ checked, onChange, label, disabled, size = 'md' }: { checked: boolean; onChange: (v: boolean) => void; label: string; disabled?: boolean; size?: 'sm' | 'md' }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      title={label}
      disabled={disabled}
      class={cx('toggle', `toggle-${size}`, checked && 'is-on')}
      onClick={(e) => {
        e.stopPropagation()
        onChange(!checked)
      }}
    >
      <span class="toggle-knob" />
    </button>
  )
}

// ---------- avatar with live ring ----------

const MONO: Record<string, string> = { claude: 'C', codex: 'X', gemini: 'G', deepseek: 'D', antigravity: 'A', opencode: 'O' }

export function agentMonogram(agent?: string): string {
  if (!agent) return '·'
  return MONO[agent.toLowerCase()] ?? agent.slice(0, 1).toUpperCase()
}

export function Avatar({ agent, state, size = 28, label, badge }: { agent?: string; state?: AgentState; size?: number; label?: string; badge?: 'standin' | 'limited' }) {
  const tone = agentTone(state)
  const r = size / 2 - 2
  const c = 2 * Math.PI * r
  const ringClass = cx('avatar-ring', state && `ring-${state}`, !state && 'ring-none')
  const title = label ?? `${agent ?? ''} · ${agentStateLabel(state)}`
  return (
    <span class="avatar" style={{ width: `${size}px`, height: `${size}px`, '--tone': toneVar(tone) } as JSX.CSSProperties} title={title} role="img" aria-label={title}>
      <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`} aria-hidden="true">
        <circle class="avatar-face" cx={size / 2} cy={size / 2} r={r - 2.5} />
        <circle
          class={ringClass}
          cx={size / 2}
          cy={size / 2}
          r={r}
          stroke-dasharray={state === 'working' ? `${c * 0.68} ${c}` : state === 'stalled' ? '2.6 2.6' : state === 'starting' ? '1 3.4' : undefined}
          style={{ transformOrigin: 'center' }}
        />
      </svg>
      <span class="avatar-mono" style={{ fontSize: `${Math.round(size * 0.4)}px` }}>
        {agentMonogram(agent)}
      </span>
      {badge && <span class={cx('avatar-badge', `badge-${badge}`)} aria-hidden="true">{badge === 'standin' ? <Icon name="swap" size={9} /> : '!'}</span>}
    </span>
  )
}

export function StateDot({ state, size = 8 }: { state?: AgentState; size?: number }) {
  return <span class={cx('state-dot', state && `dot-${state}`)} style={{ width: `${size}px`, height: `${size}px`, '--tone': toneVar(agentTone(state)) } as JSX.CSSProperties} title={agentStateLabel(state)} />
}

// ---------- progress ----------

export function ProgressBar({ value, total, tone = 'ok', label, thin }: { value: number; total: number; tone?: Tone; label?: string; thin?: boolean }) {
  const p = total > 0 ? Math.min(1, value / total) : 0
  return (
    <div class={cx('progress', thin && 'progress-thin')} role="progressbar" aria-valuemin={0} aria-valuemax={total} aria-valuenow={value} aria-label={label}>
      <div class="progress-fill" style={{ width: `${p * 100}%`, background: toneVar(tone) }} />
    </div>
  )
}

export function SegmentedBar({ segments, label }: { segments: { tone: Tone; title?: string; current?: boolean }[]; label?: string }) {
  return (
    <div class="segbar" role="img" aria-label={label}>
      {segments.map((s, i) => (
        <span key={i} class={cx('seg', `tone-${s.tone}`, s.current && 'is-current')} title={s.title} />
      ))}
    </div>
  )
}

export function QuotaBar({ used, limited }: { used?: number; limited?: boolean }) {
  if (used == null && !limited) return null
  const p = limited ? 1 : Math.max(0, Math.min(1, used ?? 0))
  const tone: Tone = limited ? 'bad' : p > 0.85 ? 'wait' : 'idle'
  return (
    <div class="quota" title={limited ? t('quotaLimited') : t('quotaUsed', { p: Math.round(p * 100) })}>
      <div class="quota-fill" style={{ width: `${p * 100}%`, background: toneVar(tone === 'idle' ? 'neutral' : tone) }} />
    </div>
  )
}

// ---------- layout pieces ----------

export function Card({ children, class: cls, interactive, onClick, tone, ...rest }: { children: ComponentChildren; class?: string; interactive?: boolean; tone?: Tone; onClick?: (e: MouseEvent) => void } & Omit<JSX.HTMLAttributes<HTMLDivElement>, 'onClick'>) {
  return (
    <div class={cx('card', interactive && 'card-interactive', tone && `card-tone-${tone}`, cls)} onClick={onClick} {...rest}>
      {children}
    </div>
  )
}

export function SectionHeader({ title, count, actions, id, tone }: { title: string; count?: number; actions?: ComponentChildren; id?: string; tone?: Tone }) {
  return (
    <div class="section-header">
      <h2 id={id} class="section-title">
        {title}
      </h2>
      {count != null && <CountBadge n={count} tone={tone} />}
      <span class="grow" />
      {actions}
    </div>
  )
}

export function EmptyState({ icon, title, hint, action, compact }: { icon: IconName; title: string; hint?: string; action?: ComponentChildren; compact?: boolean }) {
  return (
    <div class={cx('empty', compact && 'empty-compact')}>
      <span class="empty-icon">
        <Icon name={icon} size={compact ? 16 : 20} />
      </span>
      <div class="empty-title">{title}</div>
      {hint && <p class="empty-hint">{hint}</p>}
      {action && <div class="empty-action">{action}</div>}
    </div>
  )
}

export function ErrorState({ error, onRetry }: { error: string; onRetry?: () => void }) {
  return (
    <div class="error-state" role="alert">
      <Icon name="alert" size={18} />
      <div class="col" style={{ gap: '4px' }}>
        <div class="error-title">{t('loadFailed', { error })}</div>
        <p class="muted">{t('workerDown')}</p>
        {onRetry && (
          <div>
            <Button size="sm" icon="refresh" onClick={onRetry}>
              {t('retry')}
            </Button>
          </div>
        )}
      </div>
    </div>
  )
}

export function Skeleton({ lines = 3, height }: { lines?: number; height?: number }) {
  return (
    <div class="skeleton" aria-busy="true" aria-label={t('loading')}>
      {Array.from({ length: lines }, (_, i) => (
        <span key={i} class="skeleton-line" style={{ width: `${92 - ((i * 23) % 40)}%`, height: height ? `${height}px` : undefined }} />
      ))}
    </div>
  )
}

// ---------- tabs ----------

export function Tabs<K extends string>({ tabs, value, onChange, label }: { tabs: { id: K; label: string; count?: number; icon?: IconName }[]; value: K; onChange: (k: K) => void; label: string }) {
  const refs = useRef<(HTMLButtonElement | null)[]>([])
  const onKey = (e: KeyboardEvent, i: number) => {
    const d = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0
    if (!d) return
    e.preventDefault()
    const n = (i + d + tabs.length) % tabs.length
    onChange(tabs[n].id)
    refs.current[n]?.focus()
  }
  return (
    <div class="tabs" role="tablist" aria-label={label}>
      {tabs.map((tab, i) => (
        <button
          key={tab.id}
          ref={(el) => (refs.current[i] = el)}
          role="tab"
          type="button"
          aria-selected={tab.id === value}
          tabIndex={tab.id === value ? 0 : -1}
          class={cx('tab', tab.id === value && 'is-active')}
          onClick={() => onChange(tab.id)}
          onKeyDown={(e) => onKey(e, i)}
        >
          {tab.icon && <Icon name={tab.icon} size={14} />}
          <span>{tab.label}</span>
          {tab.count != null && <span class="tab-count num">{tab.count}</span>}
        </button>
      ))}
    </div>
  )
}

// ---------- popover menu ----------

export interface MenuItem {
  id: string
  label: string
  icon?: IconName
  hint?: string
  danger?: boolean
  disabled?: boolean
}

function useOutside(ref: RefObject<HTMLElement>, open: boolean, onClose: () => void) {
  useEffect(() => {
    if (!open) return
    const down = (e: PointerEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose()
    }
    const key = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation()
        onClose()
      }
    }
    document.addEventListener('pointerdown', down, true)
    document.addEventListener('keydown', key, true)
    return () => {
      document.removeEventListener('pointerdown', down, true)
      document.removeEventListener('keydown', key, true)
    }
  }, [open])
}

export function Menu({ trigger, items, onSelect, align = 'start', label, heading }: { trigger: (p: { open: boolean; toggle: () => void; id: string }) => ComponentChildren; items: MenuItem[]; onSelect: (id: string) => void; align?: 'start' | 'end'; label: string; heading?: string }) {
  const [open, setOpen] = useState(false)
  const [active, setActive] = useState(0)
  const [flip, setFlip] = useState(false)
  const root = useRef<HTMLDivElement>(null)
  const list = useRef<HTMLDivElement>(null)
  const id = useId()
  const close = useCallback(() => setOpen(false), [])
  useOutside(root, open, close)
  useLayoutEffect(() => {
    if (!open || !list.current) return
    const r = list.current.getBoundingClientRect()
    setFlip(r.bottom > window.innerHeight - 8 && r.top - r.height > 40)
    list.current.focus()
  }, [open])
  const enabled = items.map((it, i) => (!it.disabled ? i : -1)).filter((i) => i >= 0)
  const onKey = (e: KeyboardEvent) => {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault()
      const pos = enabled.indexOf(active)
      const next = enabled[(pos + (e.key === 'ArrowDown' ? 1 : -1) + enabled.length) % enabled.length]
      setActive(next ?? 0)
    } else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault()
      const it = items[active]
      if (it && !it.disabled) {
        setOpen(false)
        onSelect(it.id)
      }
    } else if (e.key === 'Tab') setOpen(false)
  }
  return (
    <div class="menu-root" ref={root}>
      {trigger({
        open,
        id,
        toggle: () => {
          setActive(enabled[0] ?? 0)
          setOpen((o) => !o)
        },
      })}
      {open && (
        <div ref={list} id={id} class={cx('menu', `menu-${align}`, flip && 'menu-up')} role="menu" aria-label={label} tabIndex={-1} onKeyDown={onKey} aria-activedescendant={`${id}-${active}`}>
          {heading && <div class="menu-heading">{heading}</div>}
          {items.map((it, i) => (
            <div
              key={it.id}
              id={`${id}-${i}`}
              role="menuitem"
              aria-disabled={it.disabled || undefined}
              class={cx('menu-item', i === active && 'is-active', it.danger && 'is-danger', it.disabled && 'is-disabled')}
              onPointerEnter={() => !it.disabled && setActive(i)}
              onClick={() => {
                if (it.disabled) return
                setOpen(false)
                onSelect(it.id)
              }}
            >
              {it.icon && <Icon name={it.icon} size={14} />}
              <span class="grow ellipsis">{it.label}</span>
              {it.hint && <span class="menu-hint">{it.hint}</span>}
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

/** A floating panel anchored under its trigger (for small forms). */
export function Popover({ open, onClose, children, align = 'start', class: cls }: { open: boolean; onClose: () => void; children: ComponentChildren; align?: 'start' | 'end'; class?: string }) {
  const ref = useRef<HTMLDivElement>(null)
  useOutside(ref, open, onClose)
  if (!open) return null
  return (
    <div ref={ref} class={cx('popover', `menu-${align}`, cls)} role="dialog">
      {children}
    </div>
  )
}

// ---------- drawer ----------

export function Drawer({ open, onClose, title, subtitle, children, actions, width = 400, inline, labelledBy }: { open: boolean; onClose: () => void; title: ComponentChildren; subtitle?: ComponentChildren; children: ComponentChildren; actions?: ComponentChildren; width?: number; inline?: boolean; labelledBy?: string }) {
  const ref = useRef<HTMLElement>(null)
  const id = useId()
  useEffect(() => {
    if (!open) return
    const key = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && !e.defaultPrevented) onClose()
    }
    document.addEventListener('keydown', key)
    return () => document.removeEventListener('keydown', key)
  }, [open, onClose])
  if (!open) return null
  return (
    <aside ref={ref} class={cx('drawer', inline ? 'drawer-inline' : 'drawer-overlay')} style={{ width: `${width}px` }} aria-labelledby={labelledBy ?? id} role="complementary">
      <header class="drawer-head">
        <div class="col grow" style={{ gap: '2px' }}>
          <h2 id={labelledBy ?? id} class="drawer-title">
            {title}
          </h2>
          {subtitle && <div class="drawer-sub">{subtitle}</div>}
        </div>
        <IconButton icon="x" label={t('close')} onClick={onClose} />
      </header>
      <div class="drawer-body">{children}</div>
      {actions && <footer class="drawer-foot">{actions}</footer>}
    </aside>
  )
}

// ---------- form bits ----------

export function Field({ label, hint, error, children, inline }: { label: string; hint?: string; error?: string; children: ComponentChildren; inline?: boolean }) {
  return (
    <label class={cx('field', inline && 'field-inline')}>
      <span class="field-label">{label}</span>
      {children}
      {error ? <span class="field-error">{error}</span> : hint && <span class="field-hint">{hint}</span>}
    </label>
  )
}

export function TextInput({ value, onInput, placeholder, mono, invalid, class: cls, ...rest }: { value: string; onInput: (v: string) => void; placeholder?: string; mono?: boolean; invalid?: boolean; disabled?: boolean } & Omit<JSX.HTMLAttributes<HTMLInputElement>, 'onInput' | 'value'>) {
  return <input class={cx('input', mono && 'mono', invalid && 'is-invalid', cls as string)} value={value} placeholder={placeholder} onInput={(e) => onInput((e.target as HTMLInputElement).value)} aria-invalid={invalid || undefined} {...rest} />
}

export function TextArea({ value, onInput, placeholder, rows = 3, mono, class: cls, ...rest }: { value: string; onInput: (v: string) => void; placeholder?: string; rows?: number; mono?: boolean; disabled?: boolean } & Omit<JSX.HTMLAttributes<HTMLTextAreaElement>, 'onInput' | 'value' | 'rows'>) {
  return <textarea class={cx('input', 'textarea', mono && 'mono', cls as string)} rows={rows} value={value} placeholder={placeholder} onInput={(e) => onInput((e.target as HTMLTextAreaElement).value)} {...rest} />
}

export function Select({ value, onChange, options, label }: { value: string; onChange: (v: string) => void; options: { value: string; label: string }[]; label?: string }) {
  return (
    <span class="select">
      <select value={value} aria-label={label} onChange={(e) => onChange((e.target as HTMLSelectElement).value)}>
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
      <Icon name="chevronDown" size={14} />
    </span>
  )
}

// ---------- toasts ----------

interface ToastItem {
  id: number
  text: string
  tone: Tone
}
const toasts = signal<ToastItem[]>([])
let toastSeq = 0

export function toast(text: string, tone: Tone = 'neutral') {
  const id = ++toastSeq
  toasts.set([...toasts.get(), { id, text, tone }].slice(-3))
  setTimeout(() => toasts.set(toasts.get().filter((x) => x.id !== id)), tone === 'bad' ? 6000 : 3200)
}

export function Toasts() {
  const [list, setList] = useState(toasts.get())
  useEffect(() => toasts.subscribe(setList), [])
  return (
    <div class="toasts" role="status" aria-live="polite">
      {list.map((x) => (
        <div key={x.id} class={cx('toast', `tone-${x.tone}`)}>
          {x.tone === 'bad' ? <Icon name="alert" size={14} /> : x.tone === 'ok' ? <Icon name="check" size={14} /> : <span class="chip-dot" />}
          <span>{x.text}</span>
        </div>
      ))}
    </div>
  )
}

/** Run an async action with a toast on failure. Returns whether it succeeded. */
export async function act(fn: () => Promise<unknown>, okText?: string): Promise<boolean> {
  try {
    await fn()
    if (okText) toast(okText, 'ok')
    return true
  } catch (err) {
    toast((err as Error)?.message || String(err), 'bad')
    return false
  }
}
