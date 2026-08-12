// Copyright © 2026 Jalapeno Labs

// Core
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'

/** One crumb. The current page carries no `to` and renders as plain text. */
export type Breadcrumb = {
  label: string
  to?: string
}

type Props = {
  items: Breadcrumb[]
}

/**
 * The trail a page hands `AppShell`, rendered above the page content.
 *
 * Crumbs are page-provided rather than derived from the route, because a show
 * page's last crumb is the record's own name, which only the page has. Every
 * scaffolded page carries one, so the frame stays the single place that decides
 * how a trail looks.
 */
export function Breadcrumbs(props: Props) {
  const { t } = useTranslation()

  return <nav aria-label={t('common.breadcrumbs')}>
    <ol className='level-left gap-2 text-sm opacity-70'>
      {
        props.items.map((item, index) => (
          <li key={item.label} className='level-left gap-2'>
            { index > 0
              ? <span aria-hidden='true'>/</span>
              : null
            }
            { item.to
              ? <Link to={item.to} className='text-primary'>{
                  item.label
                }</Link>
              : <span>{
                  item.label
                }</span>
            }
          </li>
        ))
      }
    </ol>
  </nav>
}
