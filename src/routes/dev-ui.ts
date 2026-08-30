import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import type { FastifyPluginAsync } from 'fastify';
import { config } from '../config.js';
import { ProxyError } from '../errors.js';
import { PLATFORMS, PLATFORM_LABELS, REGIONS, platformToRegion } from '../riot/routing.js';

/**
 * A single-page browser client for poking at the API by hand — the thing you
 * actually want when a payload looks wrong and `curl | jq` is three commands
 * away from an answer.
 *
 * It is registered only when `config.devUi` is on (never in production unless
 * asked for explicitly), and it is served from this origin so the browser's
 * `fetch` calls are same-origin and the service needs no CORS layer.
 *
 * The page itself is public: it is inert HTML, and the `/v1` calls it makes
 * still carry the caller's own key.
 */
const UI_FILE = fileURLToPath(new URL('../../public/dev-ui.html', import.meta.url));

const devUiRoutes: FastifyPluginAsync = async (fastify) => {
  /**
   * What the page cannot work out for itself: whether it needs to ask for a
   * key at all, and which platforms this build knows about.
   */
  fastify.get('/dev/config.json', { config: { public: true } }, async (_request, reply) => {
    reply.header('Cache-Control', 'no-store');
    return {
      authDisabled: config.authDisabled,
      defaultPlatform: config.DEFAULT_PLATFORM,
      regions: REGIONS,
      platforms: PLATFORMS.map((value) => ({
        value,
        label: PLATFORM_LABELS[value],
        region: platformToRegion(value),
      })),
    };
  });

  /**
   * `/dev/NinjaGoldfinch-OCENZ` is a client-side route, so every path under
   * `/dev` returns the same document and the page reads the URL itself.
   */
  const serve = async (_request: unknown, reply: import('fastify').FastifyReply) => {
    let html: string;
    try {
      html = await readFile(UI_FILE, 'utf8');
    } catch {
      // The production image does not ship `public/`, so this is the honest
      // answer when someone turns DEV_UI on in a container.
      throw ProxyError.notFound('The dev UI is enabled but public/dev-ui.html is not on disk');
    }
    // Re-read and re-send every time: the whole point is editing the file and
    // hitting reload.
    reply.header('Cache-Control', 'no-store').type('text/html; charset=utf-8');
    return html;
  };

  fastify.get('/dev', { config: { public: true } }, serve);
  fastify.get('/dev/*', { config: { public: true } }, serve);
};

export default devUiRoutes;
