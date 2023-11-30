#!/bin/bash

cat > /genesis.json <<EOL
{
  "config": {
    "ethash": {},
    "chainId": 7,
    "homesteadBlock": 0,
    "eip150Block": 0,
    "eip155Block": 0,
    "eip158Block": 0,
    "byzantiumBlock": 0,
    "constantinopleBlock": 0,
    "petersburgBlock": 0,
    "istanbulBlock": 0,
    "berlinBlock": 0,
    "yolov2Block": 0,
    "yolov3Block": 0,
    "londonBlock": 0,
    "mergeNetsplitBlock": 0,
    "terminalTotalDifficulty": 0,
    "shanghaiTime": 0,
    "cancunTime": 0,
    "terminalTotalDifficultyPassed": true
  },
  "nonce": "0x0000000000000033",
  "timestamp": "0x0",
  "parentHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
  "gasLimit": "0x2fefd8",
  "difficulty": "0x100",
  "mixhash": "0x0000000000000000000000000000000000000000000000000000000000000000",
  "coinbase": "0x3333333333333333333333333333333333333333",
  "alloc": {}
}
EOL

# Need to reference the datadir of init to the dev mode so it does not use an ephemeral DB
geth init --datadir=/root/.ethereum /genesis.json

exec geth --datadir=/root/.ethereum "$@"
