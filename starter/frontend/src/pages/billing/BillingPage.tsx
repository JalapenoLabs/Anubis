// Copyright © 2026 Jalapeno Labs

// Core
import { useTranslation } from 'react-i18next'
import { useParams, useSearchParams } from 'react-router'
import useSWR from 'swr'
import { useAnubisApi, useCurrentUser } from '@jalapenolabs/anubis'
import { useTeamContext } from '../../context/TeamProvider'

// UI
import { Card, CardBody, Spinner } from '@heroui/react'
import { AppShell } from '../../components/AppShell'
import { CurrentPlanCard } from '../../components/billing/CurrentPlanCard'
import { PlansGrid } from '../../components/billing/PlansGrid'

// Misc
import { ADMIN_ROLE, BILLING_ROLE } from '../../permissions'
import { getOrganizationSettingsUrl } from '../../urls'

/**
 * Query parameter Stripe returns the browser on, with `success` or `canceled`.
 *
 * The backend puts it on the URLs it hands Stripe before the purchase begins,
 * so nothing in it is trusted: a completed checkout is confirmed by Stripe's
 * own event, and this only decides which sentence the page opens with.
 */
const CHECKOUT_PARAM = 'checkout'

/**
 * What one organization is paying for: its plan, its seats, and the plans it could move to.
 *
 * The route is `/organizations/:organizationId/billing`, which is where Stripe
 * returns the browser after a checkout or a portal visit, so a cold load has to
 * work. Reading is open to every member of the organization, because the plan
 * explains what the whole organization can do; buying takes the `admin` or
 * `billing` role, which is exactly what the endpoints enforce.
 */
export function BillingPage() {
  const { t } = useTranslation()
  const api = useAnubisApi()
  const { user } = useCurrentUser()
  const { memberships } = useTeamContext()
  const { organizationId = '' } = useParams()
  const [ searchParams ] = useSearchParams()

  const organization = memberships.organizations.find(
    (candidate) => candidate.id === organizationId,
  )

  // Somebody invited into a single team appears under its organization with no
  // organization roles at all, and the billing surface is closed to them:
  // asking for it would only earn a 404.
  const isOrganizationMember = Boolean(organization?.roles.length)

  const billing = useSWR(
    isOrganizationMember ? `billing/${organizationId}` : null,
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

  const canManage = organization.roles.some(
    (role) => role === ADMIN_ROLE || role === BILLING_ROLE,
  )
  const checkoutOutcome = searchParams.get(CHECKOUT_PARAM)

  return <AppShell
    user={user}
    breadcrumbs={[
      { label: organization.name, to: getOrganizationSettingsUrl(organization.id) },
      { label: t('billing.title') },
    ]}
  >
    <div className='relaxed'>
      <h2 className='title'>{
          t('billing.title')
        }</h2>
      <p className='opacity-70'>{
          t('billing.subtitle', { organization: organization.name })
        }</p>
    </div>

    { checkoutOutcome
      ? <Card className='relaxed p-2'>
          <CardBody>
            <p>{
                checkoutOutcome === 'success'
                  ? t('billing.checkoutSuccess')
                  : t('billing.checkoutCanceled')
              }</p>
          </CardBody>
        </Card>
      : null
    }

    { isOrganizationMember
      ? null
      : <p className='relaxed opacity-70'>{
          t('organization.settings.teamMemberOnly')
        }</p>
    }

    { billing.isLoading
      ? <Spinner />
      : null
    }

    { billing.data
      ? <>
          <CurrentPlanCard
            organizationId={organization.id}
            overview={billing.data}
            canManage={canManage}
            onReconciled={() => billing.mutate()}
          />
          { billing.data.billingEnabled
            ? <PlansGrid
                organizationId={organization.id}
                currentPlanKey={billing.data.plan.key}
                canManage={canManage}
              />
            : <Card className='relaxed p-2'>
                <CardBody>
                  <h3 className='compact text-xl font-semibold'>{
                      t('billing.disabledTitle')
                    }</h3>
                  <p className='opacity-70'>{
                      t('billing.disabledBody', { plan: billing.data.plan.name })
                    }</p>
                </CardBody>
              </Card>
          }
        </>
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
