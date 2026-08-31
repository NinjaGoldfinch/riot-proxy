/**
 * §11 — the socket's own limits. A leaf module so the reference can quote them
 * without importing `src/ws/index.ts`, which pulls in the Redis subscriber and
 * the metrics registry to read three integers.
 */

/** How often the server pings an idle socket. */
export const HEARTBEAT_MS = 30_000;

/** Consecutive unanswered pings before the connection is dropped. */
export const MAX_MISSED_PONGS = 2;

/** Ceiling on one socket's subscriptions; further topics are silently ignored. */
export const MAX_TOPICS_PER_SOCKET = 200;
