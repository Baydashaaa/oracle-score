#!/usr/bin/env bash
set -euo pipefail
KEY=oracle-dev
C=$(cat .contract-rebel2)
GAS="--gas auto --gas-adjustment 1.6 --gas-prices 28.325uluna -y"

echo "==> store $(grep -m1 '^version' Cargo.toml)"
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
echo "    code_id: $NEW"

echo "==> verify checksum"
terrad q wasm code-info "$NEW" | python3 -c "
import json,sys
on=json.load(sys.stdin)['checksum'].lower()
want=open('artifacts/checksums.txt').read().split()[0].lower()
print('    on-chain:', on)
sys.exit(0 if on==want else 'CHECKSUM MISMATCH')
"

echo "==> migrate"
h=$(terrad tx wasm migrate "$C" "$NEW" '{}' --from $KEY $GAS \
    | python3 -c "import json,sys; print(json.load(sys.stdin)['txhash'])")
sleep 8
terrad q tx "$h" | python3 -c "
import json,sys
d=json.load(sys.stdin)
if d.get('code',0)!=0: sys.exit('MIGRATE FAILED: '+d.get('raw_log','')[:250])
print('    ok')
"

terrad q wasm contract-state smart "$C" '{"config":{}}'
