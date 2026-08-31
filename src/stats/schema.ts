import { Type, type Static } from '@sinclair/typebox';

/**
 * The metrics snapshot — one shape for both transports. `GET /v1/admin/metrics`
 * returns it and the `metrics` WebSocket topic ticks it, built by the same
 * function (`src/stats/snapshot.ts`), so the two can never disagree.
 *
 * A leaf module like `src/events/topics.ts`, and for the same reason: the event
 * catalogue's examples derive their type from this schema, and rendering the
 * reference must not open a connection.
 *
 * Counters are cumulative on the wire, not rates — the window belongs to
 * whoever asks (see the `cacheReadsTotal` comment in `src/metrics.ts`). A
 * dashboard diffs successive snapshots for a rate over exactly the interval it
 * cares about.
 */

/** One BullMQ queue's population, keyed by the states an operator watches. */
export const QueueCounts = Type.Object({
  active: Type.Integer({ description: 'Running right now' }),
  waiting: Type.Integer(),
  prioritized: Type.Integer({ description: 'Waiting, in the priority set (backfills, archives)' }),
  delayed: Type.Integer(),
  failed: Type.Integer({ description: 'Exhausted their retries; retained 24 h' }),
  completed: Type.Integer({
    description: 'Retained 1 h / 1000 jobs, so this is recent, not total',
  }),
});

export const MetricsSnapshot = Type.Object(
  {
    /** Bumped only when a field changes meaning; additions do not bump it. */
    v: Type.Literal(1),
    keyScope: Type.String(),
    totals: Type.Object({
      archivedMatches: Type.Integer(),
      trackedPlayers: Type.Integer(),
      knownPlayers: Type.Integer({ description: 'Every player ever seen under this key scope' }),
      activeConsumers: Type.Integer({ description: 'Consumer keys not revoked' }),
    }),
    queues: Type.Record(Type.String(), QueueCounts, {
      description: 'One entry per BullMQ queue',
    }),
    ws: Type.Object(
      {
        connections: Type.Integer(),
        subscriptions: Type.Integer({ description: 'Topics held, summed over all sockets' }),
      },
      { description: 'This api instance only — gauges are per-process' },
    ),
    events: Type.Record(Type.String(), Type.Integer(), {
      description: 'Events relayed since this api instance started, by event name. Cumulative.',
    }),
    cache: Type.Object(
      {
        hit: Type.Integer(),
        miss: Type.Integer(),
        neg: Type.Integer({ description: 'Cached upstream 404s served' }),
        stale: Type.Integer({ description: 'Served stale while a refresh ran' }),
      },
      { description: 'Cacheable reads since this api instance started, by outcome. Cumulative.' },
    ),
    limiter: Type.Array(
      Type.Object({
        scope: Type.String({ description: 'A platform or region host, e.g. `euw1`, `europe`' }),
        frozenMs: Type.Number({
          description: 'Time until a 429-induced freeze lifts; 0 when open',
        }),
        windows: Type.Array(
          Type.Object({
            window: Type.String({ description: '`limit:seconds`, as Riot states it' }),
            used: Type.Integer(),
            limit: Type.Integer(),
          }),
        ),
      }),
      { description: 'One entry per rate-limit scope this deployment has talked to' },
    ),
    process: Type.Object({
      uptimeSeconds: Type.Number(),
      rssBytes: Type.Integer(),
    }),
  },
  { $id: 'MetricsSnapshot' },
);

export type MetricsSnapshotData = Static<typeof MetricsSnapshot>;
