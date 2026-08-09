#!/usr/bin/env bash
set -euo pipefail

HOME_DIR=~/.terra-mainnet
KEY=oracle-score-admin
C=$(cat .contract-mainnet)
LCD=https://terra-classic-lcd.publicnode.com
GAS="--gas auto --gas-adjustment 1.6 --gas-prices 28.325uluna -y"
WANT=$(awk '{print tolower($1)}' artifacts/checksums.txt)

# Mainnet RPC drops connections on reads, so confirmations go through LCD.
wait_tx () {
  local body
  for _ in $(seq 1 25); do
    body=$(curl -s "$LCD/cosmos/tx/v1beta1/txs/$1" || true)
    if echo "$body" | grep -q '"tx_response"'; then echo "$body"; return 0; fi
    sleep 4
  done
  echo "timeout waiting for $1" >&2; return 1
}

echo "==> $(grep -m1 '^version' Cargo.toml)"
echo "    local checksum: $WANT"

echo "==> store"
h=$(terrad tx wasm store artifacts/oracle_score.wasm --from $KEY --home $HOME_DIR $GAS \
    | python3 -c "import json,sys; print(json.load(sys.stdin)['txhash'])")
echo "    txhash: $h"

NEW=$(wait_tx "$h" | python3 -c "
import json,sys
r=json.load(sys.stdin)['tx_response']
if r.get('code',0)!=0: sys.exit('STORE FAILED: '+r.get('raw_log','')[:250])
for e in r.get('events',[]):
    if e['type']=='store_code':
        for a in e['attributes']:
            if a['key']=='code_id': print(a['value'])
")
echo "    code_id: $NEW"

echo "==> verify on chain"
curl -s "$LCD/cosmwasm/wasm/v1/code/$NEW" | python3 -c "
import json,sys,base64
d=json.load(sys.stdin)['code_info']['data_hash']
try:
    raw=base64.b64decode(d, validate=True); on=raw.hex() if len(raw)==32 else d.lower()
except Exception:
    on=d.lower()
print('    on-chain:', on)
sys.exit(0 if on=='$WANT' else 'CHECKSUM MISMATCH — aborting')
"

echo "==> migrate"
h=$(terrad tx wasm migrate "$C" "$NEW" '{}' --from $KEY --home $HOME_DIR $GAS \
    | python3 -c "import json,sys; print(json.load(sys.stdin)['txhash'])")
wait_tx "$h" | python3 -c "
import json,sys
r=json.load(sys.stdin)['tx_response']
if r.get('code',0)!=0: sys.exit('MIGRATE FAILED: '+r.get('raw_log','')[:250])
print('    ok')
"

echo "==> config"
curl -s "$LCD/cosmwasm/wasm/v1/contract/$C/smart/eyJjb25maWciOnt9fQ==" | python3 -m json.tool
