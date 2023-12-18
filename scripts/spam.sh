#!/bin/bash

# Define a function to get the nth character in the alphanumeric sequence
get_char() {
    local index=$1
    if [ "$index" -lt 26 ]; then
        # Get one of the 26 letters
        printf "\\$(printf '%03o' $((97 + index)))"
    else
        # Get one of the 10 digits (0-9)
        printf "$((index - 26))"
    fi
}

# Loop through the first 36 alphanumeric characters (26 letters + 10 digits)
for i in {0..35}; do
    char=$(get_char $i)

    # Write 10000 bytes of the current character
    printf "%.0s$char" {1..10000} > "to_send.txt"

    # Run a command (replace this with your actual command)
    echo "Running a command for $char"

    cargo run --bin blobsender -- --url=https://blobshare.up.railway.app --priv-key=$PRIVKEY --eth-provider=$ETH_PROVIDER --data to_send.txt --skip-wait

    # Example: Uncomment the next line to list the current directory (replace `ls` with your command)
    # ls
done
