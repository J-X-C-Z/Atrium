// The project command center (custom editor "board").
import { useState } from 'preact/hooks'
import type { ProjectView } from '../../../worker/src/protocol'
import { request } from '../orca'
import { useLink, useResource, useTick } from '../data'
import { t } from '../i18n'
import { Button, Chip, EmptyState, ErrorState, Skeleton, Tabs, Toggle, act } from '../design/components'
import { Icon } from '../design/icons'
import { Markdown } from '../design/markdown'
import { basename } from '../util'
import { OrgChart } from './OrgChart'
import { Milestones } from './Milestones'
import { Activity } from './Activity'
import { Kanban } from './Kanban'
import { Team } from './Team'
import './board.css'

type Tab = 'command' | 'tasks' | 'team'

export function Board() {
  const link = useLink()
  if (link.status === 'loading')
    return (
      <div class="bd-pad">
        <Skeleton lines={5} height={16} />
      </div>
    )
  if (link.status === 'error')
    return (
      <div class="bd-pad">
        <EmptyState icon="alert" title={t('linkError')} hint={link.error} />
      </div>
    )
  if (link.link.kind !== 'board')
    return (
      <div class="bd-pad">
        <EmptyState icon="alert" title={t('linkWrong')} />
      </div>
    )
  return <ProjectBoard root={link.link.root} />
}

function ProjectBoard({ root }: { root: string }) {
  useTick(30000)
  const [res, reload] = useResource<ProjectView>(() => request('project.get', { root }) as Promise<ProjectView>, [root], {
    'project.changed': (p) => p.root === root,
    'agent.changed': () => true,
    'needs.changed': () => true,
  })
  const [tab, setTab] = useState<Tab>('command')
  const [briefOpen, setBriefOpen] = useState(false)

  if (res.status === 'loading')
    return (
      <div class="bd-pad">
        <Skeleton lines={6} height={18} />
      </div>
    )
  if (res.status === 'error')
    return (
      <div class="bd-pad">
        <ErrorState error={res.error} onRetry={reload} />
      </div>
    )
  const p = res.data
  const briefLine = p.brief.replace(/^#+\s.*$/gm, '').replace(/[*_`>#-]/g, '').trim().split('\n').find((l) => l.trim()) ?? ''

  return (
    <div class="bd">
      <header class="bd-top">
        <div class="bd-top-row">
          <span class="bd-mark" aria-hidden="true">
            <Icon name="layers" size={16} />
          </span>
          <div class="bd-titles">
            <h1 class="bd-name ellipsis">{p.name}</h1>
            <div class="bd-root mono ellipsis faint" title={p.root}>
              {p.root.startsWith('/') ? p.root : basename(p.root)}
            </div>
          </div>
          {p.needs > 0 && <Chip tone="wait" icon="alert">{t('projectNeeds', { n: p.needs })}</Chip>}
          <span class="grow" />
          <label class="bd-auto" title={p.autopilot ? t('autopilotOn') : t('autopilotOff')}>
            <Toggle checked={p.autopilot} label={t('autopilot')} onChange={(v) => act(() => request('project.autopilot', { root, on: v }))} />
            <span class="bd-auto-text">
              <span class="bd-auto-name">{t('autopilot')}</span>
              <span class="faint bd-auto-state">{p.autopilot ? t('autopilotOnShort') : t('autopilotOffShort')}</span>
            </span>
          </label>
        </div>

        <div class={briefOpen ? 'bd-brief is-open' : 'bd-brief'}>
          {briefOpen ? (
            <div class="bd-brief-body">
              {p.brief.trim() ? <Markdown text={p.brief} /> : <p class="muted">{t('briefEmpty')}</p>}
            </div>
          ) : (
            <p class="bd-brief-line ellipsis muted">{briefLine || t('briefEmpty')}</p>
          )}
          <Button size="sm" variant="quiet" iconRight={briefOpen ? 'up' : 'chevronDown'} onClick={() => setBriefOpen(!briefOpen)} aria-expanded={briefOpen}>
            {briefOpen ? t('hideBrief') : t('showBrief')}
          </Button>
        </div>

        <div class="bd-tabs">
          <Tabs
            label={p.name}
            value={tab}
            onChange={setTab}
            tabs={[
              { id: 'command', label: t('tabCommand'), icon: 'route' },
              { id: 'tasks', label: t('tabTasks'), icon: 'grid', count: p.tasks.length },
              { id: 'team', label: t('tabTeam'), icon: 'users', count: p.team.length },
            ]}
          />
          <span class="grow" />
          {res.refreshing && <span class="bd-sync" aria-hidden="true" />}
        </div>
      </header>

      <main class="bd-main" role="tabpanel">
        {tab === 'command' && (
          <div class="bd-command">
            <div class="bd-col">
              <OrgChart p={p} />
              <Milestones p={p} />
            </div>
            <Activity p={p} />
          </div>
        )}
        {tab === 'tasks' && <Kanban p={p} />}
        {tab === 'team' && <Team p={p} />}
      </main>
    </div>
  )
}
