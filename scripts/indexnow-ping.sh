#!/usr/bin/env bash
# Ping IndexNow (Bing / Yandex / Seznam / Naver) with every URL in the live
# sitemap. Run after any deploy that adds or meaningfully changes pages.
# No account needed: the key file at https://riz.dev/<key>.txt proves domain
# ownership. Bing's index is what ChatGPT search reads, so this is the lever
# that makes riz.dev retrievable by answer engines.
set -euo pipefail

HOST="riz.dev"
KEY="4985848369e62099a12aa83c521e56fe"
SITEMAP="https://${HOST}/sitemap.xml"

urls_json=$(curl -fsS "$SITEMAP" | python3 -c '
import sys, json, xml.etree.ElementTree as ET
ns = "{http://www.sitemaps.org/schemas/sitemap/0.9}"
urls = [e.text for e in ET.parse(sys.stdin).getroot().iter(ns + "loc")]
print(json.dumps(urls))
')

echo "Submitting $(echo "$urls_json" | python3 -c 'import sys,json;print(len(json.load(sys.stdin)))') URLs from $SITEMAP"

payload=$(python3 - "$urls_json" <<EOF
import sys, json
print(json.dumps({
    "host": "$HOST",
    "key": "$KEY",
    "keyLocation": "https://$HOST/$KEY.txt",
    "urlList": json.loads(sys.argv[1]),
}))
EOF
)

code=$(curl -sS -o /tmp/indexnow-response.txt -w "%{http_code}" \
  -X POST "https://api.indexnow.org/indexnow" \
  -H "Content-Type: application/json; charset=utf-8" \
  -d "$payload")

echo "IndexNow response: HTTP $code"
cat /tmp/indexnow-response.txt 2>/dev/null || true
# 200 = submitted, 202 = accepted (key validation pending). Both are success.
[[ "$code" == "200" || "$code" == "202" ]]
