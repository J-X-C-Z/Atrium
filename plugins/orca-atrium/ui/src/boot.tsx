// Shared entry: language from the worker, then mount.
import { render, type ComponentType } from 'preact'
import './design/base.css'
import './design/components.css'
import './views.css'
import { request } from './orca'
import { setLang, lang } from './i18n'
import { Toasts } from './design/components'

export async function boot(App: ComponentType, target: HTMLElement = document.getElementById('root')!) {
  try {
    const o = await Promise.race([request('overview.get'), new Promise<never>((_, rej) => setTimeout(() => rej(new Error('timeout')), 2500))])
    setLang(o.lang)
  } catch {
    setLang(lang())
  }
  render(
    <>
      <App />
      <Toasts />
    </>,
    target,
  )
}
