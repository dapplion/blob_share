# ethereum/client-go:v1.13.5
ARG baseimage=ethereum/client-go
ARG tag=latest

FROM $baseimage:$tag as builder

FROM alpine:latest
RUN apk add --update bash curl jq
COPY --from=builder /usr/local/bin/geth /usr/local/bin/geth

ADD entrypoint.sh /entrypoint.sh
ADD genesis.json /genesis.json
ENTRYPOINT ["/entrypoint.sh"]

