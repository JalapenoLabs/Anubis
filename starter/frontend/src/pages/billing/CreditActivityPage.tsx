// Copyright © 2026 Jalapeno Labs

// Core
import { useTranslation } from 'react-i18next'
import { Navigate, useParams } from 'react-router'
import useSWR from 'swr'
import { useAnubisApi, useCurrentUser } from '@jalapenolabs/anubis'
import { useTeamContext } from '../../context/TeamProvider'

// UI
import { Spinner } from '@heroui/react'
import { AppShell } from '../../components/AppShell'
import { CreditActivityCard } from '../../components/billing/CreditActivityCard'

// Misc
import { SUBSCRIPTION_SUBJECT, allows } from '@jalapenolabs/anubis'
import { getOrganizationBillingUrl, getOrganizationSettingsUrl } from '../../urls'

/**
 * The organization's credit ledger, a level below the billing screen.
 *
 * Gated on `Billing::Subscription.update`, which is what the ledger endpoint
 * enforces: a spend names the record it paid for, and a member may not reach
 * every record the organization pays for. Anyone else reads a sentence rather
 * than a request that would come back `403`.
 *
 * The entries carry no currency, so the page reads the billing overview first,
 * under the billing screen's own cache key: arriving from that screen, it is
 * already there.
 */
export function CreditActivityPage() {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { user } = useCurrentUser()
  const { memberships } = useTeamContext()
  const { organizationId = '' } = useParams()

  const organization = memberships.organizations.find(
    (candidate) => candidate.id === organizationId,
  )
  const canManage = Boolean(
    organization?.isMember && allows(organization.access, 'update', SUBSCRIPTION_SUBJECT),
  )

  const billing = useSWR(
    canManage ? `billing/${organizationId}` : null,
    () => api.getBilling(organizationId),
  )

  // RequireAuth guarantees a user before this page renders.
  if (!user) {
    return null
  }

  if (!organization) {
    return <AppShell user={user}>
      { memberships.isLoading
        ? <Spinner />
        : <p className='opacity-70'>{
            t('organization.settings.notFound')
          }</p>
      }
    </AppShell>
  }

  // An application that sells no credit has no ledger; one level up is the
  // billing screen, which is where the link here would have been.
  if (billing.data && !billing.data.wallet) {
    console.debug('CreditActivityPage found no wallet, returning to the billing screen', organizationId)
    return <Navigate to={getOrganizationBillingUrl(organization.id)} replace />
  }

  return <AppShell
    user={user}
    breadcrumbs={[
      { label: organization.name, to: getOrganizationSettingsUrl(organization.id) },
      { label: t('billing.title'), to: getOrganizationBillingUrl(organization.id) },
      { label: t('billing.activity.breadcrumb') },
    ]}
  >
    <div className='relaxed'>
      <h2 className='title'>{
          t('billing.activity.title')
        }</h2>
    </div>

    { canManage
      ? null
      : <p className='relaxed opacity-70'>{
          t('billing.activity.billingRoleOnly')
        }</p>
    }

    { billing.isLoading
      ? <Spinner />
      : null
    }

    { billing.data?.wallet
      ? <CreditActivityCard
          organizationId={organization.id}
          currency={billing.data.wallet.currency}
        />
      : null
    }

    { billing.error
      ? <p className='relaxed text-danger'>{
          t('common.somethingWentWrong')
        }</p>
      : null
    }
  </AppShell>
}
