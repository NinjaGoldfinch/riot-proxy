import { Type } from '@sinclair/typebox';
import type { FastifyPluginAsync } from 'fastify';
import { ProxyError } from '../errors.js';
import {
  DATA_FILES,
  currentVersion,
  fetchVersions,
  readMeta,
  readStatic,
} from '../static/ddragon.js';
import { applyCacheHeaders } from './helpers.js';
import { PassthroughResponse, localErrors } from './schemas.js';

/**
 * §6.2 — the local Data Dragon mirror. These routes never make an upstream
 * call and never touch the limiter (§5.6).
 */
export const FILE_ALIASES: Record<string, string> = {
  champions: 'champion',
  items: 'item',
  runes: 'runesReforged',
  'summoner-spells': 'summoner',
  'profile-icons': 'profileicon',
  maps: 'map',
};

const staticRoutes: FastifyPluginAsync = async (fastify) => {
  fastify.get(
    '/v1/static/versions',
    { schema: { tags: ['static'], response: { 200: PassthroughResponse, ...localErrors } } },
    async (_request, reply) => {
      const version = await currentVersion();
      const mirrored = await readStatic('versions', version);
      applyCacheHeaders(reply, mirrored ? 'HIT' : 'MISS', 0);
      // Fall back to the live list when nothing has been synced yet, so a fresh
      // deployment is usable before the first `ddragon:sync` run.
      return { current: version ?? null, versions: mirrored ?? (await fetchVersions()) };
    },
  );

  /**
   * Riot's queue table (#115). Its own route rather than a `DATA_FILES` entry,
   * because it is not one: a different host, no version in its path, and no
   * entry in `versions.json`. Folding it into `/v1/static/{file}` would mean
   * accepting a `?version=` for a file that has no versions and answering the
   * same bytes whatever was asked for — a documented lie for the sake of one
   * fewer route.
   *
   * This is the home #52 left owed. `queue` was removed from the mirror's file
   * list back then for naming a file Data Dragon does not serve; the data was
   * always real, it just lives somewhere else.
   */
  fastify.get(
    '/v1/static/queues',
    {
      schema: {
        tags: ['static'],
        summary: "Riot's queue id table",
        description:
          'Maps `queueId` to a map and a description — what `420` means where the archive and ' +
          'the composite routes report one. Refreshed on every `ddragon:sync`, not only on a ' +
          'new patch: Riot adds queue ids when a game mode ships, which is not a patch event.',
        response: { 200: PassthroughResponse, ...localErrors },
      },
    },
    async (_request, reply) => {
      const queues = await readMeta('queues');
      if (queues === undefined) {
        throw ProxyError.notFound(
          'The queue table has not been synced yet. Run the ddragon:sync job.',
        );
      }
      applyCacheHeaders(reply, 'HIT', 0);
      return queues;
    },
  );

  fastify.get(
    '/v1/static/:file',
    {
      schema: {
        tags: ['static'],
        params: Type.Object({
          file: Type.Unsafe<string>({
            type: 'string',
            enum: [...DATA_FILES, ...Object.keys(FILE_ALIASES)],
          }),
        }),
        querystring: Type.Object({
          // A patch number and nothing else. `version` becomes a path segment in
          // the mirror, so anything looser lets `..` walk out of DDRAGON_DIR;
          // this fails closed with VALIDATION before the filesystem is touched.
          version: Type.Optional(
            Type.String({ maxLength: 20, pattern: '^[0-9]+(\\.[0-9]+)*$', examples: ['16.17.1'] }),
          ),
        }),
        response: { 200: PassthroughResponse, ...localErrors },
      },
    },
    async (request, reply) => {
      const { file } = request.params as { file: string };
      const { version } = request.query as { version?: string };
      const resolved = FILE_ALIASES[file] ?? file;

      const data = await readStatic(resolved, version);
      if (data === undefined) {
        throw ProxyError.notFound(
          `Static data '${file}' has not been synced yet. Run the ddragon:sync job.`,
        );
      }
      applyCacheHeaders(reply, 'HIT', 0);
      return data;
    },
  );
};

export default staticRoutes;
