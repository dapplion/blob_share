#!/bin/bash

docker run --rm --entrypoint=eth2-testnet-genesis \
        -v $PWD:/data \
        skylenet/ethereum-genesis-generator capella \
        --eth1-block "{{beacon_genesis_eth1data_blockhash | default("0x0000000000000000000000000000000000000000000000000000000000000000")}}" \
        --timestamp "{{beacon_config['MIN_GENESIS_TIME']}}" \
        --config /data/config.yaml \
        --mnemonics /data/mnemonics.yaml \
        --tranches-dir /data/tranches \
        --state-output /data/genesis.ssz
