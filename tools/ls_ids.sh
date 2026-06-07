#!/usr/bin/env bash
# Read-only: lists Lemon Squeezy store / product / variant IDs so you can fill
# the constants at the top of src/license.rs. Reads the API key from the env.
#
#   export LEMONSQUEEZY_API_KEY=...   # test or live key (Settings → API)
#   ./tools/ls_ids.sh
set -euo pipefail
: "${LEMONSQUEEZY_API_KEY:?export LEMONSQUEEZY_API_KEY=... first}"

base="https://api.lemonsqueezy.com/v1"
auth="Authorization: Bearer ${LEMONSQUEEZY_API_KEY}"
acc="Accept: application/vnd.api+json"

for ep in stores products variants; do
  echo "=== ${ep} ==="
  curl -s "${base}/${ep}" -H "${auth}" -H "${acc}" | python3 -c '
import sys, json
d = json.load(sys.stdin)
if "errors" in d:
    print("ERROR:", json.dumps(d["errors"])); sys.exit(1)
rows = d.get("data", [])
if not rows:
    print("(none)")
for x in rows:
    a = x.get("attributes", {})
    keys = ["name","slug","price","price_formatted","is_subscription",
            "interval","status","product_id","store_id"]
    print(x.get("type"), x.get("id"), "|",
          {k: a.get(k) for k in keys if a.get(k) is not None})
'
done
