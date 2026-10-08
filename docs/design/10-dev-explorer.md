# 10 — Dev explorer (`/dev`)

| | |
|---|---|
| **Status** | Accepted (owner, task DEV-01, ADR-071) |
| **Date** | 2026-10-05 |
| **Replaces** | v1's `public/dev-ui.html` player viewer (P4-06) |

One self-contained page, served only outside production, that exercises every feature the proxy has and shows the raw request and response for each call. It is a troubleshooting tool for whoever runs the proxy, not a product surface.

## Principles

- **The spec is the inventory.** Every API route registers through `utoipa` (`routes/docs.rs::api_router`), so the explorer builds its forms from the OpenAPI document. A new route shows up without UI work.
- **Hand-written HTML only where it adds something:** status, a player view, the live WebSocket log and request history.
- **One file, no build.** It uses inline CSS and JS with no external URLs, and is embedded with `include_str!` like the dashboard.
- **Raw first.** Every rendered card can show the exact response: status, timing, size, headers and body.

## Gating

`/dev*` exists only when the process is not in production. Unlike v1, `DEV_UI=true` cannot turn it on in production, because the explorer can fire admin `POST`/`DELETE` calls.

```mermaid
flowchart TD
  A[Config::load] --> B{ENV == production?}
  B -- yes --> OFF["dev_ui = false<br/>(DEV_UI=true ignored, warn logged)"]
  B -- no --> C{DEV_UI set?}
  C -- "false" --> OFF
  C -- "unset / true" --> ON[dev_ui = true]
  ON --> R["routes::ui::router mounts<br/>/dev · /dev/showcase · /dev/config.json · /dev/openapi.json<br/>routes::dev::router mounts /dev/reset (admin)"]
  OFF --> N["no /dev routes →<br/>fallback 404 envelope"]
```

## What the page talks to

All calls are same-origin; the proxy has no CORS layer. `/dev/openapi.json` is the same document as `/openapi.json`, but it is served even when `DOCS_UI=false`.

```mermaid
flowchart LR
  subgraph Browser["/dev — single dev-ui.html (no deps, no build)"]
    H[Header: key · env/version · AUTH DISABLED badge · X-Request-Id override]
    T1[Explorer<br/>forms from OpenAPI]
    T2[Status<br/>health · readyz · admin metrics]
    T3[Player<br/>rendered profile + matches]
    T4[Live<br/>WS topics + frame log]
    T5[History<br/>last 50, replay]
    V[[Shared response viewer<br/>status · ms · bytes · key headers ·<br/>all headers · Pretty / Raw · copy curl]]
    T1 & T2 & T3 & T5 --> V
  end
  subgraph Proxy["riot-proxy (same origin)"]
    C["/dev/config.json"]
    O["/dev/openapi.json<br/>(served even if DOCS_UI=false)"]
    API["/v1/* read + admin<br/>/healthz /readyz /metrics"]
    WS["/v1/ws?token="]
    DD["/ddragon/* (local icons)"]
  end
  H --> C
  T1 --> O
  V -. fetch .-> API
  T4 --> WS
  T3 --> DD
```

## One explorer request

```mermaid
sequenceDiagram
  actor Dev
  participant UI as dev-ui.html
  participant P as riot-proxy
  participant R as Riot API
  Dev->>UI: pick operation (sidebar, grouped by tag)
  UI->>UI: render form from parameters + requestBody
  Dev->>UI: fill params, Send
  alt method != GET
    UI->>Dev: confirm("POST /v1/admin/cache/purge?")
  end
  UI->>P: fetch(url, Bearer key, X-Request-Id?)
  P->>P: auth · quota · cache / archive
  opt cache MISS
    P->>R: limited upstream call
    R-->>P: body + rate-limit headers
  end
  P-->>UI: status · X-Cache · X-Cache-Age · X-RateLimit-* · X-Request-Id · body
  UI->>UI: time it, measure bytes, push to history (localStorage)
  UI-->>Dev: viewer: chips for key headers, full header table, Pretty/Raw body, error envelope highlighted
```

## Page map

| Tab (`#hash`) | Calls | Renders |
|---|---|---|
| `explorer` | any documented operation | form from `parameters` / `requestBody`, then the response viewer |
| `status` | `/healthz`, `/readyz`, `/v1/admin/metrics` | readiness pills, totals, limiter scopes, queues, cache; optional 5 s refresh |
| `player` | `/v1/players/by-riot-id/{gameName}/{tagLine}/profile`, `/v1/players/{puuid}/matches`, `/v1/static/queues`, `/v1/lol/matches/{region}/{matchId}`, `/v1/admin/players/{puuid}/archive`, `/v1/admin/players/{puuid}/archive/matches` | profile card, ranks, top mastery, history and backfill card, recent or archived matches (DEV-02/03, below) |
| `live` | `/v1/ws` | topic picker, newest-first frame log (500 max), ping |
| `history` | — | last 50 requests (`localStorage`, without bodies); re-open or replay |
| `reset` | `GET /dev/reset`, `POST /dev/reset` | rows per fetched table, then a wipe of all of them (DEV-09, below) |

## Player tab (DEV-02)

- **Recent matches page by 10, 25 or 50**, with Prev/Next. The size is remembered in `localStorage` (`rp.dev.pageSize`). The matches API serves at most 20 per call, so the page makes 1–3 calls (`start`/`count` chunks) and joins them. Next is enabled only when every call came back full.
- **Filters**: queue, offered from Riot's `queues.json` via `/v1/static/queues` minus deprecated and unnamed queues, and `type` (`ranked`, `normal`, `tourney`, `tutorial`, as the API documents). A filter change goes back to page 1.
- **A summary over the page**: games, W/L, win rate, KDA and CS/min. Remakes are left out of the record and the averages.
- **Queue names** come from the same `queues.json`, falling back to `gameMode queueId`.
- **Clicking a match** opens its scoreboard inline: both teams, with the looked-up player marked. It closes with ×, a second click or Esc, and `raw` shows the match body.
- **Scoreboard layout** (DEV-04). One header row for the match: the patch (`info.gameVersion` cut to major.minor, with the full string on hover), the length as a clock, `X-Cache`, `raw` and ×. Then one block per team, headed by result, side (teamId 100 = Blue, 200 = Red), team K/D/A and gold. Both tables share fixed column widths so they line up. Numbers are right-aligned in tabular figures. Roles are short (Top/Jgl/Mid/Bot/Sup), and champion level has its own `lvl` column. In the match list, the result is capitalised, CS is a bare number, the length is `mm:ss`, and the end time is shown without seconds.
- **History and backfill card** (DEV-03), from `GET /v1/admin/players/{puuid}/archive`, with exact counts and no caps:
  - tracked or not, and the history walk's state (walking, queued, complete, stopped part-way, never) and depth;
  - how many matches are archived for the player, with W/L, remakes and timelines, the date range, and a count per queue;
  - the newest seen match;
  - this player's `archive:match` jobs by state (queued, running, done in the last 7 days, failed);
  - the last walk job.

  Buttons: **Browse all N archived**, Track/Untrack, and "Queue walk N deep". The last two are admin calls behind `confirm()`.
- **Source: Riot (live) or Archive (all stored)** (DEV-03). The archive source reads `GET /v1/admin/players/{puuid}/archive/matches`: one call per page (`count` up to 100), newest first, with `total`, so the pager shows "page X of Y", First and Last. It filters by queue only, and makes no Riot calls. Its rows render in the same table and scoreboard as live ones.
- **Every response window has ×**, and Esc closes the newest open thing on the current tab.
- The pure helpers sit in one marked block that `tests/dev_ui.mjs` unit-tests with `node --test`, run from `cargo test`. `tests/dom/` drives the whole page in jsdom against a fake API (`just ui-test`; CI job `test`).

## Reset tab (DEV-09)

Deletes every piece of fetched data so the proxy starts from empty, without a restart or deleting the database file (ADR-077). This is the in-app form of v1's `npm run reset:db -- --keep-consumers`.

- **Endpoint:** `GET /dev/reset` returns rows per table, in-memory cache entries, running jobs and what is kept. `POST /dev/reset` with `{"confirm":"reset"}` does the wipe. It deletes L1 first, then empties the tables in one write transaction, and answers rows deleted per table, `l1Entries`, `runningJobs` and `tookMs`. A missing or wrong `confirm` gets a `VALIDATION` 400 and deletes nothing.
- **Reachable only from the explorer:** `routes::dev::router` is merged only when `dev_ui` is on, so production and `DEV_UI=false` 404. Both methods sit behind `require_admin`, the same check as `/v1/admin/*`. The route is left out of the OpenAPI document, so no explorer form offers it.
- **Wiped:** players, matches, timelines, match facts and bans, every analytics table, ladder crawls and their legs, entries and match ids, the L2 `cache` table and L1, every `jobs` row, and `metrics_history`.
- **Kept:** `consumers` (the key in use keeps working), `limiter_state` (it mirrors Riot's live counts, and losing it could overrun the real limit with the production key), refinery's migration history, the Data Dragon files and the per-player refresh cooldowns.
- `routes::dev::WIPED` and `KEPT` list every table between them. A unit test fails if a migration adds a table without putting it in one list.
- **In-flight work:** a job that is running during the reset can still write once it finishes, and so can an L2 batch queued in the last 2 s. The tab warns when jobs are running. Refreshing the counts afterwards shows anything that came back.
- **UI:** the tab is last in the nav and shown in red. The button is disabled until `reset` is typed exactly. A `confirm()` dialog follows, then the POST. The result card lists rows deleted per table. Afterwards the field is cleared and the player tab's rendered data is dropped.

## Page bar (DEV-10)

`/dev`, `/dev/showcase` and `/dashboard` share one bar across the top: **riot-proxy · Dashboard · Dev explorer · Showcase · API docs · Metrics**, with the version and environment on the right.

- It is built once at startup by `routes::ui::pagebar` and injected at the `<!-- pagebar -->` marker in each page. Both pages stay single files with no shared asset, and the bar comes from one place.
- **It lists only pages this config serves** (`routes::ui::pages`): Dashboard under `DASHBOARD_UI`, Dev explorer and Showcase (design/11) under `dev_ui`, API docs under `DOCS_UI`, and `/metrics` always. In production the dashboard's bar never links to `/dev`.
- The current page is marked `aria-current="page"`. The bar sticks to the top while the page scrolls.
- It is a `div role="navigation"` with its own `rp-bar` class, so the pages' own `nav` rules don't restyle it. The dev page's old header links and version badge are gone, since the bar replaces them.
- `/docs` (Scalar) is third-party HTML, so it gets no bar.

## Safety

- Any non-`GET` asks for `confirm()` first. The Reset tab also needs the word typed (above).
- The key lives in `localStorage` (`rp.dev.key`). It is sent only as `Authorization: Bearer`, except for the WebSocket handshake, which requires `?token=`.
- "Copy as curl" writes `$RIOT_PROXY_KEY`, never the key itself.
