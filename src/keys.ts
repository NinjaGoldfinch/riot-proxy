/**
 * §12.1 — the shape of a consumer key. A leaf module rather than a constant in
 * `src/db/consumers.ts`, because the reference quotes this prefix in three
 * places and rendering the document must not open a connection to Postgres.
 */
export const KEY_PREFIX = 'rpx_';
