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

/** One rate-limit window's usage, as the dashboard meters it. */
const LimiterWindow = Type.Object({
  window: Type.String({ description: '`limit:seconds`, as Riot states it' }),
  used: Type.Integer(),
  limit: Type.Integer(),
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
        // A union of literals rather than the Type.Unsafe enum the route params
        // use: this schema is also run through Value.Check in tests, and
        // Type.Unsafe has no Kind for the value checker to visit.
        kind: Type.Union(
          [Type.Literal('platform'), Type.Literal('region'), Type.Literal('other')],
          {
            description:
              'Which host family the scope belongs to. `other` should never appear against the ' +
              'real API — it means the scope string is not in the routing table.',
          },
        ),
        label: Type.String({ description: 'Human-readable name, e.g. `EU West` for `euw1`' }),
        frozenMs: Type.Number({
          description: 'Time until a 429-induced freeze lifts; 0 when open',
        }),
        windows: Type.Array(LimiterWindow, {
          description: 'The app-level windows — the quota every call on this scope shares',
        }),
        methods: Type.Array(
          Type.Object({
            method: Type.String({ description: 'Endpoint id, e.g. `match.byId`' }),
            windows: Type.Array(LimiterWindow),
          }),
          {
            description:
              'Per-method windows Riot has taught this deployment, one entry per endpoint ' +
              'that has answered on this scope',
          },
        ),
      }),
      { description: 'One entry per rate-limit scope this deployment has talked to' },
    ),
    /**
     * Whether anything is consuming the queues. Queue counts alone cannot say:
     * a dead worker and an idle one both read all-zero `active` (#80).
     */
    worker: Type.Object(
      {
        alive: Type.Boolean({ description: 'A heartbeat is present and unexpired' }),
        lastSeenMs: Type.Union([Type.Integer(), Type.Null()], {
          description: 'Milliseconds since the last heartbeat; null when there is none',
        }),
      },
      { description: 'Deployment-wide, unlike `ws` — the heartbeat is one key in Redis' },
    ),
    /**
     * Work asked for, as against work done (#81). Both live in `queues` and
     * `totals` once they become jobs; these count the asking, which is where
     * the intent is — and, for the coalesced half, where it stops.
     */
    flows: Type.Object(
      {
        backfillsQueued: Type.Record(Type.String(), Type.Integer(), {
          description: '`<reason>:<queued|already-queued>`, e.g. `lookup:queued`',
        }),
        refreshClaims: Type.Record(Type.String(), Type.Integer(), {
          description: '`<part>:<claimed|coalesced>`, e.g. `profile:claimed`',
        }),
      },
      { description: 'Since this api instance started. Cumulative.' },
    ),
    process: Type.Object({
      uptimeSeconds: Type.Number(),
      rssBytes: Type.Integer(),
    }),
  },
  { $id: 'MetricsSnapshot' },
);

export type MetricsSnapshotData = Static<typeof MetricsSnapshot>;

/**
 * One point of the metrics history — a deliberately compact cut of the
 * snapshot, recorded every `METRICS_HISTORY_INTERVAL_S` seconds into a capped
 * Redis list so a dashboard opened cold can draw the last day, not just what
 * it happens to witness. Queues are summed across all queues: history answers
 * "was there a backlog at 3am", and the per-queue split for *now* is in the
 * live snapshot.
 */
export const MetricsHistoryPoint = Type.Object(
  {
    t: Type.Integer({ description: 'Epoch milliseconds when the point was recorded' }),
    totals: Type.Object({
      archivedMatches: Type.Integer(),
      trackedPlayers: Type.Integer(),
      knownPlayers: Type.Integer(),
    }),
    queues: Type.Object({
      active: Type.Integer({ description: 'Running at the instant of the point, all queues' }),
      pending: Type.Integer({
        description: 'waiting + prioritized + delayed, summed across queues',
      }),
      failed: Type.Integer(),
    }),
    cache: Type.Object(
      {
        hit: Type.Integer(),
        miss: Type.Integer(),
        neg: Type.Integer(),
        stale: Type.Integer(),
      },
      {
        description:
          'Cumulative counters of the api process that recorded the point — diff consecutive ' +
          'points for a rate, and treat a negative step as that process restarting.',
      },
    ),
  },
  { $id: 'MetricsHistoryPoint' },
);

export type MetricsHistoryPointData = Static<typeof MetricsHistoryPoint>;
