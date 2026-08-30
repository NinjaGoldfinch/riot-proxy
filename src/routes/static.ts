import { Type } from '@sinclair/typebox';
import type { FastifyPluginAsync } from 'fastify';
import { ProxyError } from '../errors.js';
import { DATA_FILES, currentVersion, fetchVersions, readStatic } from '../static/ddragon.js';
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
