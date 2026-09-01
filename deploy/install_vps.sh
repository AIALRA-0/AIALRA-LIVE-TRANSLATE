#!/usr/bin/env bash
set -Eeuo pipefail

umask 077

if (( EUID != 0 )); then
  printf 'deployment requires root\n' >&2
  exit 77
fi

release_dir="${1:?release directory is required}"
site_host="${LIVE_SITE_HOST:?LIVE_SITE_HOST is required}"
service_port="${AIALRA_PORT:-13150}"
worker_gateway_port="${AIALRA_WORKER_GATEWAY_PORT:-13151}"
app_slug="${AIALRA_APP_SLUG:-live-translate}"
app_name="${AIALRA_APP_NAME:-AIALRA 实时课程理解}"
platform_root="${AIALRA_PLATFORM_ROOT:?AIALRA_PLATFORM_ROOT is required}"
app_root="${AIALRA_APP_ROOT:-$platform_root/apps/live-translate}"
data_path="${AIALRA_DATA_PATH:-/var/lib/aialra-live-translate}"
current_link="$app_root/current"
env_file="$app_root/.env"
apps_file="$platform_root/apps/auth-gateway/apps.json"
reconcile_auth="$platform_root/apps/authentik/reconcile_authentik_callbacks.sh"
available_dir="$platform_root/config/nginx/sites-available"
enabled_dir="$platform_root/config/nginx/sites-enabled"
nginx_target="$available_dir/$site_host.conf"
nginx_link="$enabled_dir/$site_host.conf"
system_link="/etc/nginx/sites-enabled/$site_host.conf"
worker_gateway_target="$available_dir/live-translate-worker-gateway.conf"
worker_gateway_link="$enabled_dir/live-translate-worker-gateway.conf"
worker_gateway_system_link="/etc/nginx/sites-enabled/live-translate-worker-gateway.conf"
backup_root="${AIALRA_BACKUP_ROOT:-$platform_root/backups/live-translate}"
backup_dir="$backup_root/$(date -u +%Y%m%dT%H%M%SZ)-$$"
auth_service="${AIALRA_AUTH_SERVICE:-example-auth-gateway.service}"
certbot_credentials="${AIALRA_CERTBOT_CREDENTIALS:-/etc/letsencrypt/cloudflare.ini}"
previous_release=''
previous_image_id=''
previous_image_ref=''
build_id_file="$release_dir/BUILD_ID"

if [[ ! "$site_host" =~ ^[a-z0-9.-]+$ ]] || [[ ! "$service_port" =~ ^[0-9]{2,5}$ ]] || [[ ! "$worker_gateway_port" =~ ^[0-9]{2,5}$ ]]; then
  printf 'deployment hostname or port is invalid\n' >&2
  exit 64
fi

for required in \
  "$release_dir/deploy/compose.yaml" \
  "$release_dir/deploy/nginx/site.conf.template" \
  "$release_dir/deploy/nginx/site.http.conf.template" \
  "$release_dir/deploy/nginx/worker-gateway.conf.template" \
  "$build_id_file" \
  "$apps_file" \
  "$reconcile_auth"; do
  [[ -s "$required" ]]
done

build_id="$(tr -d '\r\n' < "$build_id_file")"
if [[ ! "$build_id" =~ ^[0-9a-f]{40}$ ]]; then
  printf 'BUILD_ID must contain one full lowercase main SHA\n' >&2
  exit 65
fi

available_kib="$(df -Pk "$platform_root" | awk 'NR==2 {print $4}')"
available_percent="$(df -Pk "$platform_root" | awk 'NR==2 {gsub(/%/, "", $5); print 100-$5}')"
if (( available_kib < 20 * 1024 * 1024 && available_percent < 5 )); then
  printf 'deployment requires at least 20 GiB or 5%% free space\n' >&2
  exit 75
fi

install -d -o root -g root -m 0700 "$backup_dir" "$app_root"
install -d -o 10001 -g 10001 -m 0700 "$data_path"
cp -a "$apps_file" "$backup_dir/apps.json.before"
[[ ! -e "$nginx_target" ]] || cp -a "$nginx_target" "$backup_dir/nginx.conf.before"
[[ ! -e "$worker_gateway_target" ]] || cp -a "$worker_gateway_target" "$backup_dir/worker-gateway.conf.before"
[[ ! -e "$env_file" ]] || cp -a "$env_file" "$backup_dir/env.before"
if [[ -L "$current_link" ]]; then
  previous_release="$(readlink -f "$current_link")"
  printf '%s\n' "$previous_release" > "$backup_dir/previous-release.txt"
  previous_container="$(docker compose --env-file "$env_file" -f "$previous_release/deploy/compose.yaml" ps -q core 2>/dev/null || true)"
  if [[ -n "$previous_container" ]]; then
    previous_image_id="$(docker inspect --format '{{.Image}}' "$previous_container")"
    previous_image_ref="$(docker inspect --format '{{.Config.Image}}' "$previous_container")"
    printf '%s\n' "$previous_image_id" > "$backup_dir/previous-image-id.txt"
    printf '%s\n' "$previous_image_ref" > "$backup_dir/previous-image-ref.txt"
  fi
fi

rollback() {
  local status="$?"
  trap - ERR INT TERM HUP
  set +e
  cp -a "$backup_dir/apps.json.before" "$apps_file"
  if [[ -s "$backup_dir/env.before" ]]; then
    cp -a "$backup_dir/env.before" "$env_file"
  fi
  "$reconcile_auth" >/dev/null 2>&1 || true
  if [[ -s "$backup_dir/nginx.conf.before" ]]; then
    cp -a "$backup_dir/nginx.conf.before" "$nginx_target"
  else
    rm -f -- "$nginx_target" "$nginx_link" "$system_link"
  fi
  if [[ -s "$backup_dir/worker-gateway.conf.before" ]]; then
    cp -a "$backup_dir/worker-gateway.conf.before" "$worker_gateway_target"
  else
    rm -f -- "$worker_gateway_target" "$worker_gateway_link" "$worker_gateway_system_link"
  fi
  if [[ -n "$previous_release" && -d "$previous_release" ]]; then
    docker compose --env-file "$env_file" -f "$release_dir/deploy/compose.yaml" down >/dev/null 2>&1 || true
    if [[ -n "$previous_image_id" && -n "$previous_image_ref" ]]; then
      docker image tag "$previous_image_id" "$previous_image_ref" >/dev/null 2>&1 || true
    fi
    ln -sfn "$previous_release" "$current_link"
    docker compose --env-file "$env_file" -f "$previous_release/deploy/compose.yaml" up -d >/dev/null 2>&1 || true
  else
    docker compose --env-file "$env_file" -f "$release_dir/deploy/compose.yaml" down >/dev/null 2>&1 || true
    rm -f -- "$current_link"
  fi
  nginx -t >/dev/null 2>&1 && systemctl reload nginx >/dev/null 2>&1 || true
  printf 'deployment rolled back; backup=%s\n' "$backup_dir" >&2
  exit "$status"
}
trap rollback ERR INT TERM HUP

if [[ ! -s "$env_file" ]]; then
  install -o root -g root -m 0600 /dev/null "$env_file"
  {
    printf 'LIVE_SITE_HOST=%s\n' "$site_host"
    printf 'AIALRA_PORT=%s\n' "$service_port"
    printf 'AIALRA_WORKER_GATEWAY_PORT=%s\n' "$worker_gateway_port"
    printf 'AIALRA_DATA_PATH=%s\n' "$data_path"
  } > "$env_file"
fi

grep -Eq '^AIALRA_WORKER_TOKEN_SHA256=[0-9a-f]{64}$' "$env_file"
tailscale_ip="$(tailscale ip -4 | head -n 1)"
[[ "$tailscale_ip" =~ ^100\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}$ ]]

export AIALRA_BUILD_ID="$build_id"
deployment_counts() {
  python3 - "$data_path/aialra.sqlite" <<'PY'
import sqlite3
import sys

database = sys.argv[1]
connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
active_leases = connection.execute(
    "SELECT COUNT(*) FROM recording_leases WHERE expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
).fetchone()[0]
active_jobs = connection.execute(
    "SELECT COUNT(*) FROM model_jobs WHERE status IN ('queued', 'leased')"
).fetchone()[0]
print(f"{active_leases} {active_jobs}")
PY
}

preflight_counts="$(deployment_counts)"
read -r active_leases active_jobs <<< "$preflight_counts"
if (( active_leases != 0 || active_jobs != 0 )); then
  printf 'deployment blocked: active_leases=%s active_model_jobs=%s\n' "$active_leases" "$active_jobs" >&2
  exit 69
fi

# Reject busy deployments before spending time or disk on a candidate image.
docker compose --env-file "$env_file" -f "$release_dir/deploy/compose.yaml" build

env_stage="$(mktemp "$backup_dir/.env.XXXXXX")"
grep -v '^AIALRA_BUILD_ID=' "$env_file" > "$env_stage"
printf 'AIALRA_BUILD_ID=%s\n' "$build_id" >> "$env_stage"
install -o root -g root -m 0600 "$env_stage" "$env_file"
rm -f -- "$env_stage"

if [[ -n "$previous_release" && -d "$previous_release" ]]; then
  docker compose --env-file "$env_file" -f "$previous_release/deploy/compose.yaml" stop core
fi
post_stop_counts="$(deployment_counts)"
read -r post_stop_leases post_stop_jobs <<< "$post_stop_counts"
if (( post_stop_leases != 0 || post_stop_jobs != 0 )); then
  printf 'deployment race detected: active_leases=%s active_model_jobs=%s\n' "$post_stop_leases" "$post_stop_jobs" >&2
  false
fi
for database_file in aialra.sqlite aialra.sqlite-wal aialra.sqlite-shm; do
  [[ ! -e "$data_path/$database_file" ]] || cp -a "$data_path/$database_file" "$backup_dir/$database_file.before"
done

ln -sfn "$release_dir" "$current_link"
docker compose --env-file "$env_file" -f "$release_dir/deploy/compose.yaml" up -d

# ReadWeave remains independently deployable while ETAPI stays on the private application network
readweave_container="${AIALRA_READWEAVE_CONTAINER:-readweave}"
if docker inspect "$readweave_container" >/dev/null 2>&1; then
  docker network connect aialra-live-translate_default "$readweave_container" 2>/dev/null || true
fi

for _ in {1..60}; do
  if curl -fsS --max-time 5 -H 'X-aialra-auth-proxy: 1' -H 'X-authentik-uid: deployment-health' "http://127.0.0.1:$service_port/api/v1/health" >/dev/null; then
    break
  fi
  sleep 2
done
health_json="$(curl -fsS --max-time 5 -H 'X-aialra-auth-proxy: 1' -H 'X-authentik-uid: deployment-health' "http://127.0.0.1:$service_port/api/v1/health")"
jq -e --arg build_id "$build_id" '.status == "ok" and .build_id == $build_id' <<< "$health_json" >/dev/null
core_container="$(docker compose --env-file "$env_file" -f "$release_dir/deploy/compose.yaml" ps -q core)"
image_revision="$(docker inspect --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}' "$core_container")"
[[ "$image_revision" == "$build_id" ]]

apps_stage="$(mktemp "$backup_dir/.apps.XXXXXX")"
jq \
  --arg host "$site_host" \
  --arg slug "$app_slug" \
  --arg name "$app_name" \
  '. + {($host): {slug:$slug,name:$name,style:"complex"}}' \
  "$apps_file" > "$apps_stage"
install -o root -g root -m 0644 "$apps_stage" "$apps_file"
rm -f -- "$apps_stage"
"$reconcile_auth"
systemctl restart "$auth_service"

http_stage="$(mktemp "$backup_dir/.nginx-http.XXXXXX")"
sed -e "s/__SITE_HOST__/$site_host/g" \
  "$release_dir/deploy/nginx/site.http.conf.template" > "$http_stage"
install -o root -g root -m 0644 "$http_stage" "$nginx_target"
rm -f -- "$http_stage"
ln -sfn "$nginx_target" "$nginx_link"
ln -sfn "$nginx_link" "$system_link"
nginx -t
systemctl reload nginx

if [[ ! -s "/etc/letsencrypt/live/$site_host/fullchain.pem" ]]; then
  certbot certonly \
    --dns-cloudflare \
    --dns-cloudflare-credentials "$certbot_credentials" \
    --dns-cloudflare-propagation-seconds 20 \
    --domain "$site_host" \
    --cert-name "$site_host" \
    --non-interactive \
    --agree-tos \
    --register-unsafely-without-email
fi

tls_stage="$(mktemp "$backup_dir/.nginx-tls.XXXXXX")"
sed \
  -e "s/__SITE_HOST__/$site_host/g" \
  -e "s/__SERVICE_PORT__/$service_port/g" \
  -e "s|__PLATFORM_ROOT__|$platform_root|g" \
  "$release_dir/deploy/nginx/site.conf.template" > "$tls_stage"
install -o root -g root -m 0644 "$tls_stage" "$nginx_target"
rm -f -- "$tls_stage"

worker_gateway_stage="$(mktemp "$backup_dir/.worker-gateway.XXXXXX")"
sed \
  -e "s/__TAILSCALE_IP__/$tailscale_ip/g" \
  -e "s/__WORKER_GATEWAY_PORT__/$worker_gateway_port/g" \
  -e "s/__SERVICE_PORT__/$service_port/g" \
  "$release_dir/deploy/nginx/worker-gateway.conf.template" > "$worker_gateway_stage"
install -o root -g root -m 0644 "$worker_gateway_stage" "$worker_gateway_target"
rm -f -- "$worker_gateway_stage"
ln -sfn "$worker_gateway_target" "$worker_gateway_link"
ln -sfn "$worker_gateway_link" "$worker_gateway_system_link"
nginx -t
systemctl reload nginx

probe_protected_route() {
  local -a headers=("${@}")
  local status=''
  for _ in {1..30}; do
    status="$(curl -sS --noproxy '*' --connect-timeout 5 --max-time 10 \
      --resolve "$site_host:443:127.0.0.1" \
      "${headers[@]}" \
      -o /dev/null -w '%{http_code}' "https://$site_host/" 2>/dev/null || true)"
    if [[ "$status" == '302' ]]; then
      return 0
    fi
    sleep 2
  done
  return 1
}

probe_protected_route
probe_protected_route -H 'X-Aialra-Authenticated: 1' -H 'X-Aialra-Sub: forged'
openssl x509 -in "/etc/letsencrypt/live/$site_host/fullchain.pem" -noout -checkend 604800 >/dev/null

trap - ERR INT TERM HUP
printf 'AIALRA_LIVE_TRANSLATE_DEPLOYED\n'
printf 'backup=%s\n' "$backup_dir"
