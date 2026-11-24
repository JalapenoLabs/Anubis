// Copyright © 2026 Jalapeno Labs

// Utility
import { z } from 'zod'

/**
 * A role's name, colour, and permissions, as the server accepts them.
 *
 * The server bounds a name at 50 characters on one line and stores a colour
 * as lowercase `#rrggbb`; saying so here keeps the save button honest before
 * a request is ever sent.
 */
export const roleSchema = z.object({
  name: z.string().trim().min(1).max(50).regex(/^[^\p{Cc}]*$/u),
  color: z.string().trim().regex(/^#[0-9a-fA-F]{6}$/),
  permissions: z.array(z.string()),
})
