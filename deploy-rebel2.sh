#!/usr/bin/env bash
set -euo pipefail

KEY=oracle-dev
ADMIN=terra1ufn6575ta92xmgzrllpf5cguycwd4kyyf5uetw
GAS="--gas auto --gas-adjustment 1.6 --gas-prices 28.325uluna -y"
EXPECTED=a6fb6a101a4465e38cd0cebe89d9146e4937f0637a4550732dbaed35cd92b18e

wait_tx () {
  for _ in $(seq 1 30); do
    if out=$(terrad q tx "$1" 2>/dev/null); then echo "$out"; return 0; fi
    sleep 3
  done
  echo "timeout waiting for tx $1" >&2; return 1
}

echo "==> store code"
h=$(terrad tx wasm store artifacts/oracle_score.wasm --from $KEY $GAS \
  | python3 -c "import json,sys; print(json.load(sys.stdin)['txhash'])")
echo "    txhash: $h"

code_id=$(wait_tx "$h" | python3 -c "
import json,sys
d=json.load(sys.stdin)
if d.get('code',0)!=0: sys.exit('STORE FAILED: '+d.get('raw_log','')[:300])
for e in d.get('events',[]):
    if e['type']=='store_code':
        for a in e['attributes']:
            if a['key']=='code_id': print(a['value'])
")
echo "    code_id: $code_id"

echo "==> verify hash"
terrad q wasm code-info "$code_id" | python3 -c "
import json,sys,base64
d=json.load(sys.stdin); v=d['data_hash']
on = base64.b64decode(v).hex() if len(v)==44 else v.lower()
print('    on-chain:', on)
print('    local:   ', '$EXPECTED')
sys.exit(0 if on=='$EXPECTED' else 'HASH MISMATCH')
"

echo "==> instantiate"
h=$(terrad tx wasm instantiate "$code_id" "$(cat init-rebel2.json)" \
  --label "oracle-score v0.1.0" --admin $ADMIN --from $KEY $GAS \
  | python3 -c "import json,sys; print(json.load(sys.stdin)['txhash'])")

addr=$(wait_tx "$h" | python3 -c "
import json,sys
d=json.load(sys.stdin)
if d.get('code',0)!=0: sys.exit('INSTANTIATE FAILED: '+d.get('raw_log','')[:300])
for e in d.get('events',[]):
    if e['type']=='instantiate':
        for a in e['attributes']:
            if a['key']=='_contract_address': print(a['value'])
")
echo "    contract: $addr"
echo "$addr" > .contract-rebel2

echo "==> config"
terrad q wasm contract-state smart "$addr" '{"config":{}}'
