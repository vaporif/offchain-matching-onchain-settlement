[![ci](https://github.com/vaporif/offchain-matching-onchain-settlement/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/vaporif/offchain-matching-onchain-settlement/actions/workflows/ci.yml)
[![audit](https://github.com/vaporif/offchain-matching-onchain-settlement/actions/workflows/audit.yml/badge.svg?branch=main)](https://github.com/vaporif/offchain-matching-onchain-settlement/actions/workflows/audit.yml)

# offchain-matching-onchain-settlement

A trade matching engine. Runs off chain, settles on chain.

People send buy and sell orders to a server over HTTP. The server matches buyers with sellers in memory. When trades happen, an operator batches up the results and posts them to an Ethereum smart contract, which does the actual token transfers.

Doing all of this on chain would cost gas on every single order, even the ones that never fill. So the matching happens off chain and you only pay for settlement.

## How it works

1. Users deposit ERC20 tokens into the Exchange smart contract
2. The gateway server watches for those deposits and tracks balances in a local SQLite database
3. Users send signed orders (limit or market) to the gateway over HTTP
4. The matching engine pairs up compatible orders, best price first
5. Fills go out to all connected clients over WebSocket
6. The operator bundles fills into a batch and submits it to the smart contract for settlement

## Layout

```
types/              shared types, orders, trades, deposits
matching-engine/    the order book, matching logic
settlement-core/    trait for settlement backends
settlement-evm/     the Ethereum settlement backend
gateway/            the main server, HTTP + WebSocket
cli/                operator tool for submitting batches
e2e-tests/          end to end tests
contracts/          the Solidity contracts (Foundry project)
```

## Build

```
cargo build --workspace --exclude e2e-tests
```

Or `nix build`.

## Test

Needs Foundry and Bun.

```
cargo nextest run --workspace
```

## Run

```
WS_URL=ws://localhost:8545 \
CONTRACT_ADDRESS=0x... \
BASE_TOKEN_ADDRESS=0x... \
QUOTE_TOKEN_ADDRESS=0x... \
DB_PATH=./ledger.db \
cargo run -p gateway
```

## What's missing

Still a work in progress. The order book lives in memory, so restarting the gateway loses all open orders. Only one trading pair, configured at startup. No auth on the WebSocket feed. Deposit watching on the EVM side is partially stubbed out.
