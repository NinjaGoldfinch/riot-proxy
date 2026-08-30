# Plan — OpenAPI document and a browsable API reference

Status: planned, not started. Tracking issue: #58 (stages #59–#66).

The proxy's contract currently lives in three places that can disagree: the
TypeBox schemas on each route (the only one the server actually enforces), the
consumer guide in the README, and §6 of [the spec](riot-proxy-spec.md). This
plan makes the route schemas the single source, emits an OpenAPI 3.1 document
from them, and serves that document as an interactive reference a consumer can
read and call without leaving the page.

---

## 1. Decisions

### 1.1 Scalar, not ReDoc

The work was scoped as "a ReDoc page". ReDoc was dropped after checking what
its open-source build can do: it renders a three-panel reference and
`x-codeSamples` beautifully, but it has **no interactive request console** —
executing a request from the page is a Redocly paid feature. "Try it out" was
an explicit requirement, so a ReDoc-only page could not have met it, and
ReDoc-plus-a-second-tool means two renderers, two sidebars and two places for
the styling to drift.

[`@scalar/fastify-api-reference`](https://www.npmjs.com/package/@scalar/fastify-api-reference)
(1.67.0, MIT) gives the same three-panel reading experience _and_ a built-in
API client in one Fastify plugin. Verified against the published tarball:

- The renderer is **inlined in `dist/index.js`** (3.7 MB) — no CDN fetch at
  runtime. That matters here: the service is self-hosted behind Caddy, and a
  docs page that goes blank when jsdelivr is unreachable is not a docs page.
- It reads `@fastify/swagger`'s document automatically and re-exposes it at
  `${routePrefix}/openapi.json` and `${routePrefix}/openapi.yaml`.
- `hooks: { onRequest, preHandler }` — the route can be opted out of the auth
  hook the way `/dev` is.
- `logLevel: 'silent'` — the asset requests must not each produce a §13
  request line.
- The extensions the content plan below depends on are all present in the
  bundle: `x-tagGroups`, `x-codeSamples`, `x-badges`, `x-displayName`,
  `x-internal`, `x-scalar-stability`.

One compatibility risk to clear first: the plugin depends on
`fastify-plugin@^4` while this project is on Fastify 5 / `fastify-plugin@^6`.
That is usually harmless (the constraint that bites is `fp(fn, {fastify: '4.x'})`,
which the bundle does not appear to set) but it is stage 0 of the work, not an
assumption to build on.

### 1.2 Generated at runtime, snapshotted into the repo

`@fastify/swagger` (9.8.1) builds the document from the schemas already
attached to each route, so the document cannot describe an endpoint the server
does not serve. Served live at `/openapi.json`.

A script writes the same document to a committed `openapi.json`, and CI fails
if it differs from what the code produces. This buys three things a live-only
document does not:

- A contract change shows up as a reviewable diff in the PR that causes it.
- External tooling (client generators, Postman, contract tests) can consume
  the spec from the repo without booting the service.
- Generating it needs a config that boots, which is a smoke test of its own.

Generating at runtime also means the document can quote the **running**
configuration — per-endpoint cache TTLs from `ENDPOINTS`, the refresh cooldown,
the default quota, `BULK_USAGE_CEILING`. A hand-written spec would state those
as prose and then be wrong after the first `CACHE_TTL_OVERRIDES` change.

---

## 2. What is there to document

37 routes. 26 already carry a `schema` with a `tags` entry; **11 do not**, and
those would land in the document untagged and undescribed:

| Route | File | Why it is missing |
| --- | --- | --- |
| `GET /v1/admin/consumers` | `routes/admin.ts` | Registered with `adminScope` only, no `schema` |
| `GET /v1/admin/tracked-players` | `routes/admin.ts` | Same |
| `GET /v1/admin/stats` | `routes/admin.ts` | Same |
| `GET /healthz` | `routes/health.ts` | Public, no schema |
| `GET /readyz` | `routes/health.ts` | Public, no schema |
| `GET /metrics` | `routes/health.ts` | Public, returns Prometheus text — needs a non-JSON content type |
| `GET /v1/admin/limits/:scope` | `routes/health.ts` | No schema; `:scope` is a platform *or* a region |
| `GET /dev/config.json` | `routes/dev-ui.ts` | Dev-only |
| `GET /dev`, `GET /dev/*` | `routes/dev-ui.ts` | Dev-only, returns HTML |
| `GET /v1/ws` | `ws/index.ts` | WebSocket — not expressible as an OpenAPI operation |

The three dev-UI routes and `/v1/ws` should be **hidden** (`hide: true`), not
documented as operations — a websocket upgrade described as `GET` returning
`101` is worse than absent. The WS protocol is documented as prose under its
own tag instead (stage 3).

The second gap is the response bodies. Every documented route currently
declares `200: PassthroughResponse`, which is `Type.Unsafe<unknown>({})` — a
schema that says nothing. That is the right call for the enforcement layer
(§6.1: Riot's payloads pass through unmodified, and validating Riot's shape
would break the proxy every time Riot adds a field) but it means the reference
would render every endpoint with an empty response. Resolving this without
re-introducing validation is stage 4.

---

## 3. Target shape

```
GET  /openapi.json          the document, live, public
GET  /openapi.yaml          the same, YAML
GET  /docs                  Scalar: reference + request console
npm run docs:spec           writes ./openapi.json
CI                          regenerates and fails on drift
```

New files:

```
src/docs/openapi.ts    document metadata: info, servers, tags, tagGroups,
                       securitySchemes, and the config-derived prose
src/docs/plugin.ts     registers @fastify/swagger + scalar, gated on config
src/docs/examples.ts   response examples (stage 4)
src/cli/write-spec.ts  boots the app, calls app.swagger(), writes openapi.json
openapi.json           committed artefact
```

Touched: `src/app.ts` (register the plugin before the route plugins — the
swagger plugin must see the route registrations), `src/config.ts` (`DOCS_UI`),
`src/routes/schemas.ts` (shared `$id` schemas, examples), the 11 unschema'd
routes, `package.json`, `.github/workflows/ci.yml`, `.prettierignore`,
`README.md`, `.env.example`.

---

## 4. Work breakdown

### Stage 0 — Compatibility spike (#59)

Before anything else: install `@fastify/swagger` and
`@scalar/fastify-api-reference`, register both against the real app, boot it,
and confirm the document generates and the page renders under Fastify 5.

Specifically confirm:

- No `fastify-plugin` version refusal at registration.
- `@fastify/swagger` v9 emits **OpenAPI 3.1** (`openapi: '3.1.0'`) and handles
  the TypeBox output as-is, including `Type.Unsafe` enums.
- The app's `ajv.customOptions.removeAdditional: 'all'` does not strip anything
  from the generated document (it applies to request validation, not to schema
  serialisation — verify rather than assume).
- Scalar's asset routes are exempted from the `@fastify/rate-limit` global
  (they are not `config.public`, so they will otherwise consume the caller's
  60/min quota loading a 3.7 MB bundle).

Timebox: half a day. Fallback if the plugin refuses Fastify 5: serve Scalar's
standalone bundle from `public/` behind a hand-rolled route, the way
`routes/dev-ui.ts` serves its page.

### Stage 0 — Compatibility spike (#59) — **done, passed**

Both plugins registered against the real `buildApp()` and the document
generated. Result of each check:

| Check | Result |
| --- | --- |
| `fastify-plugin` refusal under Fastify 5 | **No.** The published bundle declares `fastify: "4.x \|\| 5.x \|\| 6.x"`, so the `^4` dependency is irrelevant — it never sets a 4-only constraint |
| OpenAPI version | **3.1.0**, as hoped |
| `Type.Unsafe` enums | Survive intact — 16 platforms, 4 regions, 9 error codes, descriptions and all |
| `removeAdditional: 'all'` | Does not touch the document. `pattern`, `minLength`, `maxLength` and `description` all survive on `PuuidParam` |
| Scalar renders | Yes. `/docs` 301s to `/docs/`; the 3.7 MB bundle is served from `/docs/js/scalar.js` with no CDN reference in the HTML |

It also confirmed the numbers this plan was written against: **182** copies of
the error envelope in a 97 kB document, **35 of 36** operations with an empty
`200` body, and an empty `components/schemas` — `$id` alone does not produce a
component, `fastify.addSchema()` is required (stages 2 and 4).

Two things the plan got wrong:

1. **`logLevel: 'silent'` does not suppress the §13 request line.** That line is
   not Fastify's — it comes from the `onResponse` hook in `src/app.ts`, which
   calls the module-level `logger` directly and never consults the route's log
   level. Four injected requests produced four `request` lines with the plugin
   set to silent. Stage 5 has to skip the docs prefix in that hook instead.
2. **`/v1/ws` needs no `hide: true`.** It never reaches the document at all —
   `@fastify/websocket` does not register it as an operation `@fastify/swagger`
   can see. Ten routes appear untagged, not eleven; only the three `/dev` routes
   actually need hiding.

And one thing it got right, now measured rather than predicted: as registered,
the docs routes are **behind the auth hook and inside the quota**. With
`AUTH_DISABLED=false` all three (`/docs/`, the bundle, `/openapi.json`) return
`401`, and each carries `X-RateLimit-*` headers that decrement the caller's
allowance. `config: { public: true }` in stage 5 is load-bearing, not tidiness.

### Stage 1 — Make every route describable (#60)

Add `schema` blocks to the 11 routes above, `hide: true` to the four that
should not appear. Concretely:

- `/healthz` → `200: Type.Object({ ok: Type.Boolean() })`, tag `ops`.
- `/readyz` → `200` and `503` both `Type.Object({ ok, redis, postgres, keyScope })`.
  Document that 503 is the interesting one.
- `/metrics` → `200` with `content: text/plain`, no JSON schema. Note that
  Caddy blocks this from outside private ranges.
- `GET /v1/admin/consumers` → `Type.Object({ consumers: Type.Array(ConsumerSummary) })`.
  A real schema here, not passthrough — this is our own payload, and it must
  never grow a `keyHash` field without someone noticing.
- `GET /v1/admin/tracked-players` → `Type.Object({ players: Type.Array(PlayerSummary) })`.
- `GET /v1/admin/stats` → `Type.Object({ keyScope, archivedMatches, trackedPlayers })`.
- `GET /v1/admin/limits/:scope` → params schema whose enum is
  `[...PLATFORMS, ...REGIONS]`; response `{ scope, usage, frozenMs }`.

Note the ordering trap: adding a *response* schema to a route that had none
turns on `fast-json-stringify` for it, which silently drops undeclared fields.
Every schema added here needs its route's test to assert on the full body, and
`ConsumerSummary` / `PlayerSummary` must be checked against what
`listConsumers()` and `listPlayers()` actually return.

### Stage 2 — Shared components (#61)

`ErrorResponse` already has `$id: 'ErrorResponse'` but is inlined into all 26
routes seven times over — 182 copies of the same object in the emitted
document. Register it with `fastify.addSchema()` and reference it as
`{ $ref: 'ErrorResponse#' }`, so it appears once under
`components/schemas/ErrorResponse` and the sidebar has something to link to.

Same treatment for the reusable parameter schemas (`PlatformParam`,
`RegionParam`, `PuuidParam`, `MatchIdParam`, `GameNameParam`, `TagLineParam`)
so a consumer reads "PUUID" once rather than eleven times.

Then narrow `errorResponses`: attaching all seven statuses to every route is
convenient but dishonest — `/v1/static/{file}` cannot return `502`, and
`/healthz` cannot return `401`. Split into `upstreamErrors` (routes that call
Riot), `localErrors` (static, admin bookkeeping) and `publicErrors` (health).

### Stage 3 — The document itself (#62)

`src/docs/openapi.ts`:

- **`info.description`** — the consumer guide, as markdown. Most of it exists
  already in README §"Consumer guide" and should **move** here rather than be
  copied; the README then links to `/docs`. Covers: base URL and auth, the
  `X-Cache` / `X-Cache-Age` / `Retry-After` header table, the error-code table,
  the `key_scope` rule, and why a `MISS` with a large `X-Cache-Age` is not a
  contradiction.
- **`servers`** — `http://localhost:8080` and a `{scheme}://{host}` templated
  production entry, so the try-it console targets the reader's own deployment.
  Ordering matters: the console uses the first by default.
- **`securitySchemes`** — `bearerAuth` (http/bearer, `rpx_…`), plus a
  `tokenQuery` apiKey-in-query scheme documenting `?token=` for the WS
  handshake, marked as WS-only.
- **Per-operation `security`** — `[]` on the public routes so the page does not
  demand a key for `/healthz`; `bearerAuth` elsewhere. Admin routes additionally
  carry a description line about the `admin` scope and `ADMIN_IP_ALLOWLIST`.
- **`tags` with descriptions** and **`x-tagGroups`**:

  | Group | Tags |
  | --- | --- |
  | Player data | `players` (composites), `riot`, `lol` |
  | Static data | `static` |
  | Realtime | `ws` (prose only — see below) |
  | Operations | `ops` |
  | Administration | `admin` |

  `players` leads deliberately: the composites are what a consumer should reach
  for, and the passthrough routes are the escape hatch.

- **The `ws` tag, prose only.** OpenAPI 3.1 cannot express a WebSocket, and
  `webhooks` is the wrong shape for it (we are the server, and the transport is
  a persistent socket, not a callback). So `/v1/ws` is hidden as an operation
  and the protocol is documented as a tag description: the handshake URL and
  `?token=` auth, the `subscribe` / `unsubscribe` / `ping` client ops, the
  `ready` / `subscribed` / `pong` / `error` server ops, the topic format
  (`player:<puuid>`, `patch`), the five event payloads from
  `events/index.ts`, the 30 s heartbeat and two-missed-pong drop, and the fact
  that a player topic only ever fires for a **tracked** player. Scalar renders
  tag descriptions as full markdown, so the JSON samples survive.
  If this ever needs to be machine-readable, the answer is a separate AsyncAPI
  document, not a contortion of this one.
- **Config-derived prose.** Generate, don't type: a cache-TTL table built from
  `ENDPOINTS` (`ttlSeconds`, `negTtlSeconds`, `immutable`), the refresh cooldown
  from `REFRESH_COOLDOWN_S`, the default quota, `LOOKUP_BACKFILL_LIMIT`,
  `BULK_USAGE_CEILING`. Rendered into `info.description` and into per-tag
  descriptions.
- **`x-badges`** per operation, from the same source: `Cache 24h`,
  `Immutable · archived`, `No upstream call`, `Admin`, `Costs quota`. This is
  the thing a passthrough proxy's reference should say and a generic one cannot.

### Stage 4 — Examples and response shapes (#63)

The hard part, and the one that decides whether the page is worth having.

Riot's payloads stay unvalidated. But *documented* and *validated* are separable:
attach `examples` to the passthrough responses without attaching constraints.
Two tiers:

1. **Riot passthrough routes** (`/v1/riot/*`, `/v1/lol/*`) — a realistic example
   body per endpoint, drawn from the msw stubs the acceptance suite already
   uses, with PUUIDs and account IDs replaced by obviously-fake values. Keep the
   `200` schema as `PassthroughResponse` so nothing is enforced; the example is
   documentation only. Add a standing note per tag: *the body is Riot's,
   verbatim; the authority on its fields is Riot's own reference, linked.*
2. **Our own payloads** (composites, admin, static, health) — these are the
   proxy's contract, not Riot's, and deserve real schemas. `ProfileBody`,
   `MatchPageBody`, `BackfillNotice` and `RefreshState` are already declared as
   TypeScript interfaces in `routes/players.ts`; port them to TypeBox and derive
   the interfaces from them with `Static<>`, so the type and the document
   cannot drift. The `account`/`summoner`/`league`/`mastery` parts stay
   `Type.Unknown()` — they are Riot's — but `ageSeconds`, `warnings`,
   `refreshed`, `refreshAvailableIn`, `hasMore`, `matchIds` and `backfill` all
   become real, named, documented fields.

Also: **error examples**. One worked example per error code, showing the
envelope and the `Retry-After` header — a 429 from quota looks different from a
503 from the upstream limiter, and today only prose says so.

And **`x-codeSamples`** where the auto-generated snippet is not enough: the
composite endpoints (showing the `refresh` cooldown fields being read), and a
paging loop over `/v1/players/{puuid}/matches` driven by `hasMore`.

### Stage 5 — Serving it (#64)

`src/docs/plugin.ts`, registered from `buildApp()` **before** the route plugins.

- `DOCS_UI` config flag. Unlike `DEV_UI`, default it **on in production**: a
  published API's reference is part of the contract, not a debugging tool.
  Setting it off must remove `/docs` and `/openapi.json` both.
- Both routes marked `config: { public: true }` — a reference you need a key to
  read is a reference nobody reads. Note the consequence: the admin surface
  becomes publicly enumerable. That is already true of this README, and the
  routes are scope- and IP-gated, but it is a deliberate call and belongs in the
  security section. If it is later judged wrong, the lever is
  `x-internal: true` on the `admin` tag rather than gating the whole page.
- `logLevel: 'silent'` on the Scalar plugin so asset loads do not flood the §13
  request log, and an allowList entry so they do not consume the reader's quota.
- Caddy: `/docs` and `/openapi.json` pass through to the api container like any
  other path — no Caddyfile change needed. Confirm the `@internal` matcher does
  not catch them (it matches `/metrics` and `/readyz` exactly, so it does not).
- Dockerfile: `@scalar/fastify-api-reference` and `@fastify/swagger` go in
  `dependencies`, not `devDependencies`, or `npm prune --omit=dev` removes them
  from the runtime image. Adds ~4 MB.

### Stage 6 — The committed snapshot and CI (#65)

- `src/cli/write-spec.ts`: build the app, `await app.ready()`, `app.swagger()`,
  write `JSON.stringify(doc, null, 2) + '\n'` to `openapi.json`, close.
- `npm run docs:spec`.
- `.prettierignore` += `openapi.json` — it is generated, and prettier and the
  generator will otherwise disagree forever.
- CI step after `Typecheck`: `npm run docs:spec && git diff --exit-code openapi.json`,
  with a failure message naming the fix (`npm run docs:spec` and commit).
- The generator needs `RIOT_API_KEY` (config refuses to boot without one) but
  no Redis or Postgres — CI already provides all three, but check that
  `buildApp()` can reach `ready()` without a live Redis, since the rate-limit
  plugin and the WS hub both connect at registration. If it cannot, the step
  moves after the `Migrate` step and reuses the services.

### Stage 7 — Reconcile the prose (#66)

Once `/docs` exists, three documents describe the same API. Resolve:

- **README** — the endpoint table and the error/header tables move to the
  generated document; the README keeps Why, Quick start, How it works,
  Configuration, Deployment and Development, and links to `/docs` for the
  contract. This is a deletion, not a rewrite — the README is 38 kB and the
  consumer guide is a third of it.
- **`docs/riot-proxy-spec.md`** — untouched. It is reproduced verbatim by
  policy and is a design document, not a reference.
- **`public/dev-ui.html`** — leave it. It is a player-shaped tool ("show me
  this account"), `/docs` is an endpoint-shaped one; they do not overlap. Add
  cross-links between them.

---

## 5. Risks and open questions

| Risk | Handling |
| --- | --- |
| `@scalar/fastify-api-reference` depends on `fastify-plugin@^4` under Fastify 5 | Stage 0 spike; fallback is serving the standalone bundle by hand |
| Adding response schemas turns on `fast-json-stringify` and silently drops fields | Every stage-1 and stage-4 schema lands with a test asserting the full body |
| The reference publicly enumerates the admin surface | Deliberate; routes are scope- and IP-gated. `x-internal` on the `admin` tag is the lever if reconsidered |
| Try-it against a production deployment spends real quota and real Riot budget | The console's default server is localhost; the production server entry is templated so the reader opts in by filling it |
| Riot payload examples go stale as Riot changes fields | Examples are documentation only, never validated; each carries a "Riot's payload, verbatim" note and a link to Riot's reference |
| +4 MB to the runtime image | Accepted. Alternative is a build-time static bundle, which loses the config-derived content |
| `docs/` is in `.prettierignore` wholesale | This plan file inherits that; `openapi.json` lives at the repo root and gets its own ignore entry |

Open questions for whoever picks this up:

1. Should `/openapi.json` be served when `DOCS_UI` is off? Argument for: client
   generators want the spec without the page. Argument against: one flag, one
   behaviour. Currently planned as one flag.
2. Is `sea` worth a dedicated callout in the `riot` tag description? The routing
   quirk (account-v1 has no `sea` host, so the proxy sends it to `asia`) is the
   single most surprising thing about the API and is currently only in the README.
   Leaning yes.
3. Does the acceptance suite gain a check that every registered route appears in
   the document, so a new endpoint cannot ship undocumented? Cheap to write
   against `app.printRoutes()`. Leaning yes, as part of stage 6.
4. Two open bugs touch routes this work documents, and documenting a broken
   route is how a bug becomes a contract. #52 — `queue` is not a Data Dragon
   file, so the `queues` alias in `FILE_ALIASES` can never resolve — means the
   `/v1/static/{file}` enum currently advertises a value that always 404s.
   #51 — `?version=` escapes the Data Dragon directory — means the same route's
   `version` parameter has no real constraint to document. Both should land
   before stage 4 writes examples for that route, or the examples encode the
   bug.

---

## 6. Done means

- [ ] `GET /docs` renders every public endpoint, grouped, with descriptions.
- [ ] A reader can paste a key into the console and successfully call
      `/v1/players/by-riot-id/{gameName}/{tagLine}/profile` against a local
      instance, without editing anything else.
- [ ] Every one of the 37 routes is either documented or explicitly hidden.
- [ ] The proxy's own payloads (composites, admin, health, static) have real
      schemas; Riot's passthrough bodies have examples and no constraints.
- [ ] `openapi.json` is committed and CI fails on drift.
- [ ] The error envelope, every error code, and the cache headers are documented
      with worked examples.
- [ ] The WebSocket protocol is documented on the page, not only in the README.
- [ ] The README's consumer guide is gone, replaced by a link.
