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
app_slug="${AIALRA_APP_SLUG:-live-translate}"
app_name="${AIALRA_APP_NAME:-AIALRA 实时课程理解}"
app_root='/srv/aialra/apps/live-translate'
current_link="$app_root/current"
env_file="$app_root/.env"
apps_file='/srv/aialra/apps/auth-gateway/apps.json'
reconcile_auth='/srv/aialra/apps/authentik/reconcile_authentik_callbacks.sh'
available_dir='/srv/aialra/config/nginx/sites-available'
enabled_dir='/srv/aialra/config/nginx/sites-enabled'
nginx_target="$available_dir/$site_host.conf"
nginx_link="$enabled_dir/$site_host.conf"
system_link="/etc/nginx/sites-enabled/$site_host.conf"
backup_dir="/srv/aialra/backups/live-translate/$(date -u +%Y%m%dT%H%M%SZ)-$$"
previous_release=''

if [[ ! "$site_host" =~ ^[a-z0-9.-]+$ ]] || [[ ! "$service_port" =~ ^[0-9]{2,5}$ ]]; then
  printf 'deployment hostname or port is invalid\n' >&2
  exit 64
fi

for required in \
  "$release_dir/deploy/compose.yaml" \
  "$release_dir/deploy/nginx/site.conf.template" \
  "$release_dir/deploy/nginx/site.http.conf.template" \
  "$apps_file" \
  "$reconcile_auth"; do
  [[ -s "$required" ]]
done

install -d -o root -g root -m 0700 "$backup_dir" "$app_root"
install -d -o 10001 -g 10001 -m 0700 \
  /srv/aialra/data/live-translate \
  /srv/aialra/data/live-translate-models
install -d -o root -g root -m 0700 /srv/aialra/data/live-translate-ollama
cp -a "$apps_file" "$backup_dir/apps.json.before"
[[ ! -e "$nginx_target" ]] || cp -a "$nginx_target" "$backup_dir/nginx.conf.before"
[[ ! -e "$env_file" ]] || cp -a "$env_file" "$backup_dir/env.before"
if [[ -L "$current_link" ]]; then
  previous_release="$(readlink -f "$current_link")"
fi

rollback() {
  local status="$?"
  trap - ERR INT TERM HUP
  set +e
  cp -a "$backup_dir/apps.json.before" "$apps_file"
  "$reconcile_auth" >/dev/null 2>&1 || true
  if [[ -s "$backup_dir/nginx.conf.before" ]]; then
    cp -a "$backup_dir/nginx.conf.before" "$nginx_target"
  else
    rm -f -- "$nginx_target" "$nginx_link" "$system_link"
  fi
  if [[ -n "$previous_release" && -d "$previous_release" ]]; then
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
    printf 'AIALRA_DATA_PATH=/srv/aialra/data/live-translate\n'
    printf 'AIALRA_MODEL_PATH=/srv/aialra/data/live-translate-models\n'
    printf 'AIALRA_OLLAMA_PATH=/srv/aialra/data/live-translate-ollama\n'
    printf 'AIALRA_ASR_MODEL=small\n'
    printf 'AIALRA_ASR_DEVICE=cpu\n'
    printf 'AIALRA_ASR_COMPUTE_TYPE=int8\n'
    printf 'AIALRA_OLLAMA_MODEL=qwen2.5:1.5b-instruct\n'
  } > "$env_file"
fi

ln -sfn "$release_dir" "$current_link"
docker compose --env-file "$env_file" -f "$release_dir/deploy/compose.yaml" build
docker compose --env-file "$env_file" -f "$release_dir/deploy/compose.yaml" up -d

for _ in {1..60}; do
  if curl -fsS --max-time 5 "http://127.0.0.1:$service_port/api/v1/health" >/dev/null; then
    break
  fi
  sleep 2
done
curl -fsS --max-time 5 "http://127.0.0.1:$service_port/api/v1/health" >/dev/null

docker compose --env-file "$env_file" -f "$release_dir/deploy/compose.yaml" \
  exec -T ollama ollama pull "$(awk -F= '$1 == "AIALRA_OLLAMA_MODEL" {print $2}' "$env_file")"

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
systemctl restart aialra-auth-gateway.service

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
    --dns-cloudflare-credentials /etc/letsencrypt/cloudflare-aialra.ini \
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
  "$release_dir/deploy/nginx/site.conf.template" > "$tls_stage"
install -o root -g root -m 0644 "$tls_stage" "$nginx_target"
rm -f -- "$tls_stage"
nginx -t
systemctl reload nginx

anonymous_status="$(curl -sS --resolve "$site_host:443:127.0.0.1" -o /dev/null -w '%{http_code}' "https://$site_host/")"
[[ "$anonymous_status" == '302' ]]
forged_status="$(curl -sS --resolve "$site_host:443:127.0.0.1" -H 'X-Aialra-Authenticated: 1' -H 'X-Aialra-Sub: forged' -o /dev/null -w '%{http_code}' "https://$site_host/")"
[[ "$forged_status" == '302' ]]
openssl x509 -in "/etc/letsencrypt/live/$site_host/fullchain.pem" -noout -checkend 604800 >/dev/null

trap - ERR INT TERM HUP
printf 'AIALRA_LIVE_TRANSLATE_DEPLOYED\n'
printf 'backup=%s\n' "$backup_dir"
