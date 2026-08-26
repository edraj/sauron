#!/usr/bin/env bash
# Sauron post-upgrade runbook, as a script. Run ONCE after installing a new
# version, INSIDE tmux/screen (the first rollup backfill on a large history
# can run an hour or more, and interrupting it mid-run is the one thing this
# script cannot make safe — see step 2).
#
#   sudo bash /usr/share/doc/sauron-server/post-upgrade.sh
#
# Every step is idempotent or refuses loudly, so re-running the whole script
# after a failure is the intended recovery path — EXCEPT an interrupted
# rollup backfill, which needs SETUP.md's manual cleanup first.
set -euo pipefail

ENV_FILE=/etc/sauron/sauron.env
MIGRATE=/usr/bin/sauron-migrate

banner() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
die()    { printf '\033[31mFATAL:\033[0m %s\n' "$*" >&2; exit 1; }

# Run a sauron binary as the sauron user with the deployment env loaded.
as_sauron() {
  sudo -u sauron bash -c 'set -a; . "$1"; set +a; shift; exec "$@"' _ "$ENV_FILE" "$@"
}
# One SQL statement against DATABASE_URL, unaligned tuple output.
run_sql() {
  sudo -u sauron bash -c 'set -a; . "$1"; set +a; exec psql "$DATABASE_URL" -Atc "$2"' _ "$ENV_FILE" "$1"
}

[[ $EUID -eq 0 ]]   || die "run with sudo (needs systemctl and sudo -u sauron)"
[[ -r $ENV_FILE ]]  || die "missing $ENV_FILE"
[[ -x $MIGRATE ]]   || die "missing $MIGRATE — is sauron-server installed?"

banner "0/5 preflight: this binary must be new enough for this script"
# Older sauron-migrate builds SILENTLY IGNORED unknown subcommands — a typo'd
# or not-yet-shipped command "ran" instantly and did nothing, which cost a
# production upgrade a full day of confusion. New builds reject unknown args,
# and this probe uses exactly that: if the probe *succeeds*, the installed
# binary predates every subcommand this script is about to rely on.
if as_sauron "$MIGRATE" sauron-post-upgrade-probe >/dev/null 2>&1; then
  die "installed sauron-migrate silently accepts unknown arguments — it predates \
strict argument checking (and so the subcommand behavior this script relies \
on). Install the current release before running this script."
fi
echo "binary rejects unknown arguments: OK"

banner "1/5 apply pending migrations"
# Idempotent; fail-closed. NOTE for hosts upgrading from v1.7.3 or older with
# a LONG session history: migration 0073 rewrites the sessions table in one
# transaction and can need more lock slots than a default Postgres provides —
# see SETUP.md's 0073 section (ALTER SYSTEM SET max_locks_per_transaction)
# BEFORE running this on such a host. Hosts already at v1.7.4+ are past it.
as_sauron "$MIGRATE"

banner "post-migration: refresh sessions statistics"
# Migration 0073 rebuilds `sessions` into per-day partitions and runs no
# ANALYZE of its own, so every new child starts with empty planner statistics.
# Autovacuum does get there (the children carry threshold-5000 overrides), but
# on a long history that is hundreds of tables against a few cost-limited
# workers — and until it finishes, any plan touching sessions can misestimate
# badly enough to ride the request timeout into a 503. One explicit
# ANALYZE closes that window now; on a parent, Postgres analyzes every child.
# Idempotent, and cheap next to the backfill below.
run_sql "ANALYZE sessions" >/dev/null || die "ANALYZE sessions failed"
echo "sessions statistics refreshed"

banner "2/5 rollup backfill (skipped automatically if already done)"
echo "NOTE: first run replays all pre-existing history day by day — budget"
echo "roughly an hour per 100M events. Do NOT interrupt it: a partial backfill"
echo "must not be blindly re-run (the aggregates are additive); see SETUP.md."
as_sauron "$MIGRATE" backfill-rollups

banner "3/5 environment-scope backfills (per-app markers make re-runs no-ops)"
as_sauron "$MIGRATE" backfill-device-envs
as_sauron "$MIGRATE" backfill-person-envs

banner "4/5 start daemons and check health"
systemctl start sauron-api sauron-ingest
for unit in sauron-api sauron-ingest; do
  systemctl is-active --quiet "$unit" || die "$unit did not start — journalctl -u $unit"
  echo "$unit: active"
done
# Ports come from the deployment env; fall back to the built-in defaults.
API_PORT=$(sudo -u sauron bash -c ". $ENV_FILE 2>/dev/null; echo \${API_PORT:-8080}")
INGEST_PORT=$(sudo -u sauron bash -c ". $ENV_FILE 2>/dev/null; echo \${INGEST_PORT:-8081}")
curl -sf "http://localhost:${API_PORT}/health"    >/dev/null || die "api /health failed on :${API_PORT}"
curl -sf "http://localhost:${INGEST_PORT}/health" >/dev/null || die "ingest /health failed on :${INGEST_PORT}"
echo "api :${API_PORT}/health OK, ingest :${INGEST_PORT}/health OK"

banner "5/5 database verification"
fail=0
part=$(run_sql "SELECT COALESCE(to_regclass('sessions_default')::text, '')")
if [[ -z "$part" ]]; then
  echo "FAIL: sessions is not partitioned — migration 0073 did not apply"; fail=1
else
  echo "PASS: sessions is partitioned"
fi
dupes=$(run_sql "SELECT count(*) FROM (SELECT 1 FROM sessions GROUP BY app_id, session_id HAVING count(*) > 1) d")
if [[ "$dupes" != "0" ]]; then
  echo "FAIL: $dupes duplicate sessions — the migration-73 write path needs a look"; fail=1
else
  echo "PASS: zero duplicate sessions"
fi
marked=$(run_sql "SELECT count(*) FROM rollup_backfill")
apps=$(run_sql "SELECT count(*) FROM apps")
echo "INFO: rollup readiness markers: ${marked} (apps: ${apps})"
echo "INFO: rollup watermarks (lag grows until sauron-ingest's first fold, ~60s):"
run_sql "SELECT source || ': ' || COALESCE(date_trunc('second', now() - watermark)::text, 'never') FROM rollup_watermarks ORDER BY source"
oldest=$(run_sql "SELECT COALESCE(min(occurred_at)::date::text, 'no events') FROM analytics_events")
echo "INFO: oldest analytics event: ${oldest} (new arrivals are clamped to a 30-day floor; pre-existing garbage needs the SETUP.md repair)"
[[ $fail -eq 0 ]] || die "verification failed — see FAIL lines above"

banner "done"
echo "Optional next steps, at your pace:"
echo "  - cold tier:        systemctl enable --now sauron-tier   (size the disk behind /var/lib/sauron/cold first)"
echo "  - session retention: dashboard -> Admin -> Storage (default: off, keep forever)"
if [[ -d /etc/systemd/system/sauron-migrate.service.d ]]; then
  echo "  - note: a local sauron-migrate.service override exists; the shipped unit now"
  echo "    has TimeoutStartSec=infinity, so the override is redundant (harmless to keep)."
fi
