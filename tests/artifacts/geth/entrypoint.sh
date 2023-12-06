#!/bin/bash

# Need to reference the datadir of init to the dev mode so it does not use an ephemeral DB
geth init --datadir=/root/.ethereum /genesis.json

exec geth --datadir=/root/.ethereum "$@"
