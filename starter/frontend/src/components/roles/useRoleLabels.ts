// Copyright © 2026 Jalapeno Labs

import type {
  MemberRolesPickerLabels,
  RoleFormLabels,
  RoleLadderLabels,
} from '@jalapenolabs/anubis'

// Core
import { useTranslation } from 'react-i18next'

// Misc
import { MEMBERSHIP_SUBJECT, ROLE_SUBJECT, SUBSCRIPTION_SUBJECT } from '@jalapenolabs/anubis'

/** The framework's own subjects, by the key their name is translated under. */
const frameworkSubjectKeys: Record<string, string> = {
  [MEMBERSHIP_SUBJECT]: 'roles.subjects.membership',
  [ROLE_SUBJECT]: 'roles.subjects.role',
  [SUBSCRIPTION_SUBJECT]: 'roles.subjects.subscription',
}

/**
 * The role screens' strings, translated, for the package's role components.
 *
 * The package never imports i18next, so each of its role components takes its
 * words as a `labels` object; this is the one place the starter builds them.
 */
export function useRoleLabels() {
  const { t } = useTranslation()

  const ladder: RoleLadderLabels = {
    ladder: t('roles.ladder.ariaLabel'),
    administrator: t('roles.ladder.administrator'),
    everybody: t('roles.ladder.everybody'),
    drag: t('roles.ladder.drag'),
    moveUp: t('roles.ladder.moveUp'),
    moveDown: t('roles.ladder.moveDown'),
    pinned: t('roles.ladder.pinned'),
  }

  const form: RoleFormLabels = {
    name: t('roles.form.name'),
    color: t('roles.form.color'),
    permissions: t('roles.form.permissions'),
    subject: t('roles.form.subject'),
    administratorNote: t('roles.form.administratorNote'),
    defaultNote: t('roles.form.defaultNote'),
    notHeld: t('roles.form.notHeld'),
    actions: {
      read: t('roles.actions.read'),
      create: t('roles.actions.create'),
      update: t('roles.actions.update'),
      destroy: t('roles.actions.destroy'),
    },
  }

  const picker: MemberRolesPickerLabels = {
    roles: t('roles.picker.label'),
    refusals: {
      'outranked': t('roles.picker.outranked'),
      'lacks-permissions': t('roles.picker.lacksPermissions'),
    },
  }

  /**
   * How a permission subject reads.
   *
   * The framework's subjects are translated here. An application model reads
   * as its identifier split into words until the application adds a
   * `roles.subjects` entry for it; `::` is i18next's namespace separator, so a
   * namespaced subject is looked up with it replaced.
   */
  function subjectLabel(subject: string): string {
    const frameworkKey = frameworkSubjectKeys[subject]
    if (frameworkKey) {
      return t(frameworkKey)
    }

    const words = subject.replace(/([a-z0-9])([A-Z])/g, '$1 $2').toLowerCase()
    const readable = words.charAt(0).toUpperCase() + words.slice(1)
    return t(`roles.subjects.${subject.replaceAll('::', '_')}`, { defaultValue: readable })
  }

  return {
    ladder,
    form,
    picker,
    subjectLabel,
  } as const
}
