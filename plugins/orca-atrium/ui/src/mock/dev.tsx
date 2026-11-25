// Dev host: ?view=sidebar|board|run|template&theme=light|dark&lang=zh|en&scenario=empty|error|slow
import { render } from 'preact'
import { installMock } from './host'
import { TEMPLATE_TOML, ROOT, RUN_DIR } from './fixtures'
import { setLang } from '../i18n'
import { boot } from '../boot'
import { Sidebar } from '../sidebar/Sidebar'
import { Board } from '../board/Board'
import { RunView } from '../run/RunView'
import { TemplateEditor } from '../template/TemplateEditor'

void render
const q = new URLSearchParams(location.search)
const view = q.get('view') ?? 'sidebar'
const theme = q.get('theme') === 'dark' ? 'dark' : 'light'
const lang = q.get('lang') === 'en' ? 'en' : 'zh'
const doc = view === 'board' ? JSON.stringify({ project: q.get('project') ?? ROOT }) : view === 'run' ? JSON.stringify({ run: q.get('run') ?? RUN_DIR }) : TEMPLATE_TOML

installMock({ theme, lang, doc, scenario: q.get('scenario') ?? undefined })
setLang(lang)
document.body.dataset.view = view
const App = { sidebar: Sidebar, board: Board, run: RunView, template: TemplateEditor }[view] ?? Sidebar
document.getElementById('root')!.dataset.view = view
boot(App)
