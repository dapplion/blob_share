#!/bin/bash

# Set the initial block number to start searching from
START_BLOCK=1

# Ethereum node URL
NODE_URL="http://localhost:8545"

# Function to get the number of transactions in a block
get_tx_count() {
    block_number_hex=$(printf "%x" $1)
    tx_count=$(curl -s -X POST -H "Content-Type: application/json" \
        --data '{"jsonrpc":"2.0","method":"eth_getBlockTransactionCountByNumber","params":["0x'$block_number_hex'"],"id":1}' \
        $NODE_URL | jq -r '.result' | xargs printf "%d")
    echo $tx_count
}

# Main loop to find the block
block_number=$START_BLOCK
while true; do
    echo "Checking block number $block_number..."
    tx_count=$(get_tx_count $block_number)

    if [ "$tx_count" -eq "1" ]; then
        echo "Found block with 1 transaction: Block Number $block_number"
    fi

    block_number=$((block_number + 1))
done

