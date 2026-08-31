import { FormatRegistry } from '@sinclair/typebox';

/**
 * TypeBox checks a `format` only if the format is registered, and reports an
 * unregistered one as a validation *error* — so `Value.Errors(MetricsSnapshot,
 * snap)` fails on any timestamp it finds, and passes only while the field
 * happens to be absent. That made "the snapshot satisfies the schema" an
 * assertion about empty arrays: it held until a crawl was running, and then
 * failed on `startedAt` rather than on anything about the snapshot.
 *
 * Fastify validates responses with ajv, which knows these formats already;
 * this exists so the tests that reach for TypeBox directly check the same
 * thing the wire does.
 */
const ISO_DATE_TIME = /^\d{4}-\d{2}-\d{2}[Tt]\d{2}:\d{2}:\d{2}(\.\d+)?([Zz]|[+-]\d{2}:\d{2})$/;

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

FormatRegistry.Set('date-time', (value) => ISO_DATE_TIME.test(value));
FormatRegistry.Set('uuid', (value) => UUID.test(value));
