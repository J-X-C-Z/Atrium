// Inline SVG icon set, 16×16 grid, 1.5 stroke, currentColor.
import type { JSX } from 'preact'

const P: Record<string, string> = {
  layers: 'M8 2 14 5.2 8 8.4 2 5.2Z M2 8.2 8 11.4 14 8.2 M2 11 8 14.2 14 11',
  gear: 'M8 5.6a2.4 2.4 0 1 0 0 4.8 2.4 2.4 0 0 0 0-4.8Z M8 1.5v1.6 M8 12.9v1.6 M1.5 8h1.6 M12.9 8h1.6 M3.4 3.4l1.1 1.1 M11.5 11.5l1.1 1.1 M3.4 12.6l1.1-1.1 M11.5 4.5l1.1-1.1',
  check: 'M3.2 8.4 6.4 11.4 12.8 4.8',
  x: 'M4 4l8 8 M12 4l-8 8',
  chevronDown: 'M4 6l4 4 4-4',
  chevronRight: 'M6 4l4 4-4 4',
  chevronLeft: 'M10 4 6 8l4 4',
  sendBack: 'M6 3.5 2.5 7 6 10.5 M2.8 7H9.5a4 4 0 0 1 0 8H8',
  play: 'M5 3.2v9.6L12.6 8Z',
  pause: 'M5.5 3.5v9 M10.5 3.5v9',
  stop: 'M4 4h8v8H4Z',
  plus: 'M8 3v10 M3 8h10',
  minus: 'M3 8h10',
  terminal: 'M2.5 3.5h11v9h-11Z M5 6.5l2 1.6-2 1.6 M8.5 10h2.5',
  message: 'M2.5 3.5h11v7.5H7l-3 2.5V11H2.5Z',
  // bell
  nudge: 'M4 11.5V7.2a4 4 0 0 1 8 0v4.3l1 1H3Z M6.6 14a1.5 1.5 0 0 0 2.8 0',
  diamond: 'M8 1.8 14.2 8 8 14.2 1.8 8Z',
  grid: 'M2.5 2.5h4.5v4.5H2.5Z M9 2.5h4.5v4.5H9Z M2.5 9h4.5v4.5H2.5Z M9 9h4.5v4.5H9Z',
  clock: 'M8 2a6 6 0 1 0 0 12A6 6 0 0 0 8 2Z M8 4.8V8l2.2 1.6',
  alert: 'M8 2 14.5 13.5h-13Z M8 6.5v3.2 M8 11.6v.1',
  file: 'M4 1.8h5l3 3v9.4H4Z M9 1.8v3h3',
  flag: 'M3.5 14V2.5 M3.5 3h8l-1.8 2.8L11.5 8.6h-8',
  users: 'M6 7.2a2.4 2.4 0 1 0 0-4.8 2.4 2.4 0 0 0 0 4.8Z M1.8 13.5c.4-2.4 2-3.8 4.2-3.8s3.8 1.4 4.2 3.8 M10.8 2.6a2.3 2.3 0 0 1 0 4.4 M12 9.9c1.2.5 2 1.7 2.2 3.6',
  grip: 'M6 4h.01 M10 4h.01 M6 8h.01 M10 8h.01 M6 12h.01 M10 12h.01',
  up: 'M8 13V3 M4 7l4-4 4 4',
  down: 'M8 3v10 M4 9l4 4 4-4',
  trash: 'M2.5 4.5h11 M6.5 4.5V2.8h3v1.7 M4 4.5l.7 9h6.6l.7-9',
  refresh: 'M13 8a5 5 0 1 1-1.5-3.6 M13 2.5v3h-3',
  external: 'M9.5 2.5h4v4 M13.5 2.5 7.5 8.5 M11.5 9.5v4h-9v-9h4',
  gauge: 'M2.5 11a5.5 5.5 0 1 1 11 0 M8 11l2.6-3.4',
  swap: 'M3 5.5h9.5 M10 3l2.5 2.5L10 8 M13 10.5H3.5 M6 8l-2.5 2.5L6 13',
  megaphone: 'M2.5 6.5v3h2l5 3v-9l-5 3Z M12 5.8a3 3 0 0 1 0 4.4',
  pencil: 'M10.5 2.8l2.7 2.7L6 12.7 2.8 13.2l.5-3.2Z',
  dot: 'M8 6a2 2 0 1 0 0 4 2 2 0 0 0 0-4Z',
  undo: 'M5.5 3 2.5 6l3 3 M2.8 6H10a3.5 3.5 0 0 1 0 7H7',
  redo: 'M10.5 3l3 3-3 3 M13.2 6H6a3.5 3.5 0 0 0 0 7h3',
  link: 'M6.8 9.2a3 3 0 0 0 4.2 0l2-2a3 3 0 0 0-4.2-4.2l-.8.8 M9.2 6.8a3 3 0 0 0-4.2 0l-2 2a3 3 0 0 0 4.2 4.2l.8-.8',
  inbox: 'M2.5 9 4.3 3.5h7.4L13.5 9v3.5h-11Z M2.5 9h3.2l.8 1.5h3l.8-1.5h3.2',
  route: 'M4 13.5a1.5 1.5 0 1 0 0-3 1.5 1.5 0 0 0 0 3Z M12 5.5a1.5 1.5 0 1 0 0-3 1.5 1.5 0 0 0 0 3Z M4 10.5V8a2 2 0 0 1 2-2h4a2 2 0 0 0 2-2',
  tidy: 'M2.5 3h4v3h-4Z M9.5 3h4v3h-4Z M2.5 10h4v3h-4Z M9.5 10h4v3h-4Z',
  sparkle: 'M8 2.5v3 M8 10.5v3 M2.5 8h3 M10.5 8h3',
  maximize: 'M2.5 6V2.5H6 M10 2.5h3.5V6 M13.5 10v3.5H10 M6 13.5H2.5V10',
  minimize: 'M6 2.5V6H2.5 M13.5 6H10V2.5 M10 13.5V10h3.5 M2.5 10H6v3.5',
}

export type IconName = keyof typeof P

export function Icon({ name, size = 16, class: cls, title, ...rest }: { name: IconName; size?: number; class?: string; title?: string } & JSX.SVGAttributes<SVGSVGElement>) {
  const filled = name === 'play' || name === 'dot'
  return (
    <svg
      class={`icon ${cls ?? ''}`}
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill={filled ? 'currentColor' : 'none'}
      stroke="currentColor"
      stroke-width={name === 'grip' ? 2.2 : 1.5}
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden={title ? undefined : 'true'}
      role={title ? 'img' : undefined}
      {...rest}
    >
      {title && <title>{title}</title>}
      <path d={P[name]} />
    </svg>
  )
}
