import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import type { FastifyPluginAsync } from 'fastify';
import { config } from '../config.js';
import { ProxyError } from '../errors.js';

/**
 * The operational dashboard: live counts, queue state, limiter meters and an
 * event feed, over `GET /v1/admin/metrics` and the `metrics`/`firehose` WS
 * topics.
 *
 * Registered when `config.dashboardUi` is on — which, unlike the dev UI,
 * defaults on in production: this is an operator tool for the *deployed*
 * service, and production is where you want it. The page itself is inert HTML
 * served same-origin; every byte of data behind it requires an admin-scoped
 * key and passes the admin IP allowlist, exactly like the routes it calls.
 */
const UI_FILE = fileURLToPath(new URL('../../public/dashboard.html', import.meta.url));

const dashboardRoutes: FastifyPluginAsync = async (fastify) => {
  /** The one thing the page cannot work out: whether to ask for a key. */
  fastify.get(
    '/dashboard/config.json',
    { config: { public: true }, schema: { hide: true } },
    async (_request, reply) => {
      reply.header('Cache-Control', 'no-store');
      return { authDisabled: config.authDisabled };
    },
  );

  // Hidden from the OpenAPI document like the dev UI: the page is a tool; the
  // contract is the admin routes and WS topics it consumes, which are described.
  fastify.get(
    '/dashboard',
    { config: { public: true }, schema: { hide: true } },
    async (_request, reply) => {
      let html: string;
      try {
        html = await readFile(UI_FILE, 'utf8');
      } catch {
        throw ProxyError.notFound(
          'The dashboard is enabled but public/dashboard.html is not on disk',
        );
      }
      // Re-read per request, same as the dev UI: edit the file, hit reload.
      reply.header('Cache-Control', 'no-store').type('text/html; charset=utf-8');
      return html;
    },
  );
};

export default dashboardRoutes;
