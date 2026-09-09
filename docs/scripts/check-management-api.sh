#!/usr/bin/env bash
set -euo pipefail

base_url="${1:-http://127.0.0.1:9102}"
token="${2:?usage: check-management-api.sh [base-url] TOKEN WASM_PATH}"
wasm_path="${3:?usage: check-management-api.sh [base-url] TOKEN WASM_PATH}"
api="${base_url%/}/api/v1"
auth="Authorization: Bearer ${token}"

command -v curl >/dev/null || { echo "curl is required" >&2; exit 1; }
command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }
test -f "$wasm_path" || { echo "WASM file not found: $wasm_path" >&2; exit 1; }

health="$(curl -fsS -H "$auth" "$api/health")"
ready="$(curl -fsS -H "$auth" "$api/ready")"
test "$(jq -r '.status' <<<"$health")" = healthy
test "$(jq -r '.status' <<<"$ready")" = ready

registered="$(curl -fsS -H "$auth" -H 'Content-Type: application/wasm' \
  --data-binary "@$wasm_path" "$api/modules")"
module_id="$(jq -er '.id' <<<"$registered")"
[[ "$module_id" =~ ^[0-9a-f]{64}$ ]]

duplicate="$(curl -fsS -H "$auth" -H 'Content-Type: application/wasm' \
  --data-binary "@$wasm_path" "$api/modules")"
test "$(jq -r '.id' <<<"$duplicate")" = "$module_id"

curl -fsS -H "$auth" "$api/modules/$module_id" \
  | jq -e --arg id "$module_id" '.id == $id' >/dev/null
curl -fsS -H "$auth" "$api/modules" \
  | jq -e --arg id "$module_id" '.items | any(.id == $id)' >/dev/null
curl -fsS -X POST -H "$auth" "$api/modules/$module_id/runs" >/dev/null
curl -fsS -H "$auth" "$api/ready" | jq -e '.status == "ready"' >/dev/null
curl -fsS -H "$auth" "$api/peers" | jq -e '.items | type == "array"' >/dev/null
curl -fsS -H "$auth" "$api/keys" | jq -e '.items | type == "array"' >/dev/null

curl -fsS -X DELETE -H "$auth" "$api/modules/$module_id" >/dev/null
curl -fsS -X DELETE -H "$auth" "$api/modules/$module_id" >/dev/null
status="$(curl -sS -o /dev/null -w '%{http_code}' -H "$auth" "$api/modules/$module_id")"
test "$status" = 404

echo "Numax Management API smoke test passed: ${base_url%/}"
