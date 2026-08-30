#!/usr/bin/env bash
#
# Full local remake — `npm run reset:all`.
#
# Destroys every piece of local state the proxy owns and rebuilds it from the
# repo: Docker volumes (Redis + Postgres), build output, installed deps and the
# Data Dragon mirror. Your .env is never touched — it holds the Riot key.
#
set -euo pipefail

cd "$(dirname "$0")/.."

KEEP_DEPS=0
ASSUME_YES=0

for arg in "$@"; do
  case "$arg" in
    --keep-deps) KEEP_DEPS=1 ;;
    -y|--yes)    ASSUME_YES=1 ;;
    -h|--help)
      cat <<'USAGE'
Full local remake: destroys the Docker volumes (Redis + Postgres), dist/,
data/ and node_modules, then brings the stack back up, migrates and builds.
Your .env is never touched.

Usage: npm run reset:all [-- --keep-deps] [--yes]

  --keep-deps   leave node_modules in place (skips the reinstall)
  --yes, -y     do not ask for confirmation
USAGE
      exit 0
      ;;
    *) echo "Unknown option: $arg (try --help)" >&2; exit 2 ;;
  esac
done

if [ "${NODE_ENV:-development}" = "production" ]; then
  echo "Refusing to run: NODE_ENV=production. This script destroys data." >&2
  exit 1
fi

echo "This will destroy:"
echo "  - the riot-proxy Docker volumes (all cached responses and the match archive)"
echo "  - dist/ and data/"
if [ "$KEEP_DEPS" -eq 0 ]; then echo "  - node_modules/"; fi
echo "It will not touch .env."
echo

if [ "$ASSUME_YES" -eq 0 ]; then
  if [ ! -t 0 ]; then
    echo "Not a terminal and --yes was not passed. Aborting." >&2
    exit 1
  fi
  printf 'Continue? [y/N] '
  read -r reply
  case "$reply" in
    [yY]|[yY][eE][sS]) ;;
    *) echo "Aborted."; exit 1 ;;
  esac
fi

step() { echo; echo "── $* ──"; }

step "Tearing down containers and volumes"
docker compose down -v --remove-orphans

step "Removing build output and generated data"
rm -rf dist data
if [ "$KEEP_DEPS" -eq 0 ]; then rm -rf node_modules; fi

if [ ! -f .env ]; then
  step "Creating .env from .env.example"
  cp .env.example .env
  echo "Set RIOT_API_KEY in .env before starting the service."
fi

step "Starting redis + postgres"
# --wait blocks on the healthchecks in docker-compose.yml, so the migration
# below cannot race an unready Postgres.
docker compose up -d --wait

step "Installing dependencies"
if [ "$KEEP_DEPS" -eq 0 ]; then
  npm ci
else
  npm install
fi

step "Running migrations"
npm run migrate

step "Building"
npm run build

echo
echo "Done. Next:"
echo "  npm run key:create -- --name my-website   # mint a consumer key"
echo "  npm run dev                               # api on :8080"
echo "  npm run dev:worker                        # polling, backfill, ddragon sync"
