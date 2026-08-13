// Copyright © 2026 Jalapeno Labs

import type { RoleKey } from './roles.generated'

// Misc
import { RoleGrants } from './roles.generated'

/**
 * The role key every tenancy management endpoint asks for.
 *
 * Renaming a tenant, editing a member's roles, revoking invitations, and every
 * deletion are gated on holding this key rather than on a per-model grant,
 * because they administer the tenant itself instead of anything inside it. The
 * `RoleKey` annotation ties the constant to the compiled `config/roles.yml`:
 * drop `admin` from the file and this stops compiling, instead of silently
 * hiding every control it guards.
 */
export const ADMIN_ROLE: RoleKey = 'admin'

/** Every role a member can hold, as the generated module lists them. */
export const ROLE_OPTIONS = Object.keys(RoleGrants)
