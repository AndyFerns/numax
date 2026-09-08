---
title: Management API
description: Start a daemon and verify every authenticated v1 management endpoint by hand.
---

The Management API lets an operator inspect and manage a running Numax node
without restarting it or supplying a module to `nx serve`. Every endpoint,
including the probes, requires the configured bearer token.

## Prepare a node and a module

Build Numax and the persistent counter guest:

```bash
cargo build -p nx-cli
cargo build --release --target wasm32-unknown-unknown \
  --manifest-path examples/kv_counter/Cargo.toml
```

Create a local token and configuration:

```bash
printf '%s\n' 'replace-this-development-token' > /tmp/numax-management.token
chmod 600 /tmp/numax-management.token

cat >/tmp/numax-management.toml <<'EOF'
[management]
listen = "127.0.0.1:9102"
token_file = "/tmp/numax-management.token"
EOF
```

Start the daemon in one terminal:

```bash
DATA_DIR=$(mktemp -d /tmp/numax-management-data.XXXXXX)
cargo run -p nx-cli -- serve \
  --config /tmp/numax-management.toml \
  --datastore-path "$DATA_DIR"
```

Use these variables in a second terminal. The commands below require `curl`
and `jq`:

```bash
API=http://127.0.0.1:9102/api/v1
TOKEN=replace-this-development-token
WASM=examples/kv_counter/target/wasm32-unknown-unknown/release/kv_counter.wasm
AUTH="Authorization: Bearer $TOKEN"
```

## Verify probes and peers

Both authenticated probe aliases return `200` once a node without sync is
ready:

```bash
curl -i -H "$AUTH" "$API/health"
curl -i -H "$AUTH" "$API/ready"
```

Expected JSON is `{"status":"healthy"}` and `{"status":"ready"}`. A node
still starting returns `503`, `Retry-After: 1`, and `not_ready` from the second
call.

List connected peers. With sync disabled this is an empty page:

```bash
curl -fsS -H "$AUTH" "$API/peers" | jq
```

To exercise pagination, add `?limit=1`; when `next_cursor` is not null, pass it
unchanged:

```bash
CURSOR=$(curl -fsS -H "$AUTH" "$API/peers?limit=1" | jq -r '.next_cursor // empty')
test -z "$CURSOR" || curl -fsS -G -H "$AUTH" \
  --data-urlencode "cursor=$CURSOR" --data-urlencode 'limit=1' "$API/peers" | jq
```

## Register and inspect a module

Register the raw WASM bytes. A new artifact returns `201` and a `Location`
header; uploading the same bytes again returns `200` with the same ID:

```bash
REGISTERED=$(curl -fsS -H "$AUTH" -H 'Content-Type: application/wasm' \
  --data-binary "@$WASM" "$API/modules")
printf '%s\n' "$REGISTERED" | jq
MODULE_ID=$(printf '%s\n' "$REGISTERED" | jq -er '.id')

curl -i -H "$AUTH" -H 'Content-Type: application/wasm' \
  --data-binary "@$WASM" "$API/modules"
```

List and inspect registered modules:

```bash
curl -fsS -H "$AUTH" "$API/modules?limit=50" | jq
curl -fsS -H "$AUTH" "$API/modules/$MODULE_ID" | jq
```

Module cursors are opaque too:

```bash
CURSOR=$(curl -fsS -H "$AUTH" "$API/modules?limit=1" | jq -r '.next_cursor // empty')
test -z "$CURSOR" || curl -fsS -G -H "$AUTH" \
  --data-urlencode "cursor=$CURSOR" --data-urlencode 'limit=1' "$API/modules" | jq
```

## Run the module and read its data

Execute the registered counter once. Success is `204 No Content`:

```bash
curl -i -X POST -H "$AUTH" "$API/modules/$MODULE_ID/runs"
```

Keys are binary and therefore represented as unpadded Base64URL. List every
application key, or filter by an encoded prefix:

```bash
curl -fsS -H "$AUTH" "$API/keys" | jq

PREFIX=$(printf 'count' | base64 | tr '+/' '-_' | tr -d '=\n')
curl -fsS -G -H "$AUTH" --data-urlencode "prefix=$PREFIX" "$API/keys" | jq
```

The empty binary key is represented by `~`: read it with `GET /api/v1/keys/~`.
This marker also appears in `items`, `X-Numax-Key`, and `next_cursor` when
appropriate. Pass it unchanged as `cursor=~` to continue after the empty key.
Nonempty keys retain their unpadded Base64URL encoding. Omitting `prefix`, or
using `prefix=~`, lists all application keys.

The counter key is `counter`, whose Base64URL form is `Y291bnRlcg`. Its value
is returned as raw `application/octet-stream` bytes:

```bash
KEY=Y291bnRlcg
curl -i -H "$AUTH" "$API/keys/$KEY"
curl -fsS -H "$AUTH" "$API/keys/$KEY"
```

After the first run, the second command prints `1`. The response also includes
`X-Numax-Key: Y291bnRlcg`. Internal keys under `__nx/`, including the module
registry itself, never appear in key listings and behave as missing when read.

## Delete the module

Deletion is idempotent: both calls return `204`. Inspection and new runs return
`404` afterwards.

```bash
curl -i -X DELETE -H "$AUTH" "$API/modules/$MODULE_ID"
curl -i -X DELETE -H "$AUTH" "$API/modules/$MODULE_ID"
curl -i -H "$AUTH" "$API/modules/$MODULE_ID"
curl -i -X POST -H "$AUTH" "$API/modules/$MODULE_ID/runs"
```

## Check rejected requests

These calls verify the most important safety boundaries:

```bash
# Missing authentication: 401 plus WWW-Authenticate: Bearer
curl -i "$API/health"

# Wrong upload media type: 415
curl -i -H "$AUTH" --data-binary "@$WASM" "$API/modules"

# Invalid WASM: 422; it is not registered
printf 'not wasm' | curl -i -H "$AUTH" -H 'Content-Type: application/wasm' \
  --data-binary @- "$API/modules"

# Invalid page size and cursor: 400
curl -i -H "$AUTH" "$API/modules?limit=101"
curl -i -H "$AUTH" "$API/modules?cursor=invalid"

# Unknown route: stable JSON 404
curl -i -H "$AUTH" "$API/unknown"
```

All failures use `{"error":{"code":"...","message":"..."}}`. Uploads are
limited to 16 MiB, responses to 1 MiB, pages to 100 items, routed requests to
the configured timeout, and concurrent work to 64 requests.

Peer pages can contain fewer items to respect the response limit; follow
`next_cursor` until it is null. A peer whose identity cannot fit by itself
returns `500 internal_error` rather than an oversized response.

Running WASM yields periodically, including during its start function, so
request timeouts and shutdown can cancel a guest that loops indefinitely.
Effects already produced by the guest are not rolled back; do not automatically
retry a run after a timeout or lost connection.

## Run the repeatable smoke script

With the daemon still running, the repository script checks registration,
duplicate registration, listing, inspection, one-shot execution, key and peer
listing, readiness, and idempotent deletion:

```bash
bash docs/scripts/check-management-api.sh \
  http://127.0.0.1:9102 \
  replace-this-development-token \
  "$WASM"
```

Stop `nx serve` with `Ctrl+C` when finished.
