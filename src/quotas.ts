/**
 * §7.1 / §12.1 — the two per-minute allowances the service applies when nobody
 * has chosen one. A leaf module on purpose: the document builder reads both to
 * describe them, and it should not have to import the database layer to do it.
 */

/**
 * What a consumer gets when the operator does not choose. Mirrored by the
 * column default in `src/db/schema.ts`; changing this alone re-defaults new
 * rows but leaves existing ones, which is usually what you want.
 */
export const DEFAULT_QUOTA_PER_MIN = 600;

/**
 * What an unauthenticated caller gets. Enough to read the reference and try a
 * route from the console, not enough to be useful as an anonymous proxy.
 */
export const ANON_QUOTA_PER_MIN = 60;
