docker run --rm \
        -v=/var/folders/qj/l3ztb2cn6ss4tm4bcm6w1jbh0000gn/T/chaindata/geth/jwtsecret:/jwtsecret \
	-p=9596:9596 \
chainsafe/lodestar dev \
	--genesisValidators=1 \
	--startValidators=0..1 \
	--params.SECONDS_PER_SLOT=2 \
	--params.ALTAIR_FORK_EPOCH=0 \
	--params.BELLATRIX_FORK_EPOCH=0 \
	--params.TERMINAL_TOTAL_DIFFICULTY=0 \
	--params.CAPELLA_FORK_EPOCH=0 \
	--params.DENEB_FORK_EPOCH=0 \
	--reset \
	--rest \
	--rest.namespace="*" \
	--rest.address=0.0.0.0 \
	--rest.port=9596 \
	--jwtSecret=/jwtsecret \
	--execution.urls=http://host.docker.internal:8551 \
	--genesisEth1Hash=0xfa3b6cd3aa0637b5f0a238490a5b42939896b87de2e46f594e47e837466a3a76

