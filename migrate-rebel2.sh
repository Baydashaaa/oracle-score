#!/usr/bin/env bash
set -euo pipefail
KEY=oracle-dev
C=$(cat .contract-rebel2)
GAS="--gas auto --gas-adjustment 1.6 --gas-prices 28.325uluna -y"

send () {
  h=$(terrad tx wasm "$@" --from $KEY $GAS \
      | python3 -c "import json,sys; print(json.load(sys.stdin)['txhash'])")
  for _ in $(seq 1 20); do
    if out=$(terrad q tx "$h" 2>/dev/null); then
      echo "$out" | python3 -c "
import json,sys
d=json.load(sys.stdin)
if d.get('code',0)!=0: sys.exit('FAILED: '+d.get('raw_log','')[:250])
print('    ok', '$h'[:12])
"
      return 0
    fi
    sleep 3
  done
  echo "timeout $h" >&2; return 1
}

echo "==> store v0.2.0"
h=$(terrad tx wasm store artifacts/oracle_score.wasm --from $KEY $GAS \
    | python3 -c "import json,sys; print(json.load(sys.stdin)['txhash'])")
sleep 8
NEW=$(terrad q tx "$h" | python3 -c "
import json,sys
d=json.load(sys.stdin)
if d.get('code',0)!=0: sys.exit('STORE FAILED: '+d.get('raw_log','')[:250])
for e in d.get('events',[]):
    if e['type']=='store_code':
        for a in e['attributes']:
            if a['key']=='code_id': print(a['value'])
")
echo "    new code_id: $NEW"

echo "==> migrate"
send migrate "$C" "$NEW" '{}'

echo "==> update max_delta"
send execute "$C" '{"update_config":{"max_delta":"100000000"}}'

echo "==> rescale actions"
python3 -c "
import json
d=json.load(open('init-rebel2.json'))
for a in d['actions']:
    print(json.dumps({'set_action':{'item':a}}))
" | while read -r msg; do
  send execute "$C" "$msg"
done

echo "==> verify"
terrad q wasm contract-state smart "$C" '{"config":{}}'
terrad q wasm contract-state smart "$C" '{"ref_count":{"action":"answer","ref_id":"q2"}}'
