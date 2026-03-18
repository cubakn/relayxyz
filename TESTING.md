# Testing

## Automated tests

```bash
cargo test
```

28 integration tests covering auth, publishing, queries, replaceable events, deletion, tags, expiration, kind validation, and edge cases. All run against an in-process relay with a temp database. Rate limiting is disabled in tests.

### Stress test

```bash
cargo test --release stress -- --ignored --nocapture
```

Spawns 50 keys × 200 events = 10,000 events across 50 concurrent WebSocket connections. Each connection pre-signs all events, pipelines them without waiting for OK responses, then drains results. Finishes with query benchmarks against the populated DB.

### Benchmarking a deployed relay

`examples/stress.rs` is a standalone client binary that connects to a running relay. Use it to benchmark a real deployment.

```bash
cargo zigbuild --release --example stress --target x86_64-unknown-linux-musl
scp target/x86_64-unknown-linux-musl/release/examples/stress root@your-server:/tmp/

# on the server
./stress ws://127.0.0.1:7777 200 500   # 200 keys × 500 events = 100k
```

Arguments: `stress <relay_url> [num_keys] [events_per_key]`. Defaults to `ws://127.0.0.1:7777 100 500`.

---

## Manual testing with nak

All commands assume the relay is running locally on `ws://localhost:7777`.

```bash
cargo run
```

### Test key pairs

| Identity | Secret Key (hex) | Public Key (hex) |
|----------|-------------------|-------------------|
| Alice | `17fcb8fd6f58467365e9dc798347134240559ef0b32d31c492867895170db47d` | `fcdff5dc286939a8a33dcc5a058181f7a74b41fb880bf9771f9a9b89d94c41c0` |
| Bob | `1e77483a4a1ff1202e0ce5d0eca12b6703475147e7ae949a0b759746cf09acfa` | `e59f6847ab9defc81a70d765f1a9dfe38ae6579eb97c5bbe929b173ff198b3a1` |

Load your admin key and set up test variables:

```bash
source .env

ALICE_SEC=17fcb8fd6f58467365e9dc798347134240559ef0b32d31c492867895170db47d
ALICE_PUB=fcdff5dc286939a8a33dcc5a058181f7a74b41fb880bf9771f9a9b89d94c41c0
BOB_SEC=1e77483a4a1ff1202e0ce5d0eca12b6703475147e7ae949a0b759746cf09acfa
BOB_PUB=e59f6847ab9defc81a70d765f1a9dfe38ae6579eb97c5bbe929b173ff198b3a1
```

---

### 0. Whitelist setup (Admin API)

Before Alice or Bob can publish, their pubkeys must be whitelisted.

#### Add Alice to whitelist

```bash
curl -s -X POST http://localhost:7777/admin/pubkey \
  -H "Authorization: Bearer $RELAY_ADMIN_KEY" \
  -d '{"pubkey":"'$ALICE_PUB'"}' | jq
```

#### Add Bob to whitelist

```bash
curl -s -X POST http://localhost:7777/admin/pubkey \
  -H "Authorization: Bearer $RELAY_ADMIN_KEY" \
  -d '{"pubkey":"'$BOB_PUB'"}' | jq
```
---

### 1. NIP-11: Relay Information Document

#### Fetch relay info

**Tests:** The relay responds with a valid NIP-11 JSON document including name, supported NIPs, and limitation fields.

```bash
 nak relay ws://localhost:7777
```

#### Fetch with curl (raw)

```bash
curl -s -H "Accept: application/nostr+json" http://localhost:7777/ | jq
```

**Expected:** JSON with `name`, `supported_nips: [1, 2, 4, 9, 11, 18, 22, 25, 40, 42, 51, 65, 70]`, and `limitation.auth_required` matching `RELAY_REQUIRE_AUTH`.

---

### 2. NIP-42: Authentication

When `RELAY_REQUIRE_AUTH=true`, clients must authenticate before publishing. The relay sends an AUTH challenge on WebSocket connect. Most Nostr clients (and `nak`) handle NIP-42 automatically.

#### 2a. Publish with authentication (automatic via nak)

**Tests:** `nak` handles NIP-42 automatically when the relay sends an AUTH challenge.

```bash
nak event -k 1 -c "hello from Alice" --sec $ALICE_SEC --auth ws://localhost:7777
```

**Expected:** Event JSON printed; relay accepts with `["OK", <id>, true, ...]`.

#### 2b. Publish rejected: not authenticated

**Tests:** Without authentication, publishing is rejected.

```bash
nak event -k 1 -c "should fail" --sec $ALICE_SEC ws://localhost:7777
```

**Expected:** `["OK", <id>, false, "auth-required: authenticate to publish"]`.

---

### 3. NIP-01: Basic Protocol

#### 3a. Publish a Kind 1 (short text note)

**Tests:** An authenticated, whitelisted pubkey can publish a kind 1 event (max 180 graphemes).

```bash
nak event -k 1 -c "hello from Alice" --sec $ALICE_SEC --auth ws://localhost:7777
```

**Expected:** Event JSON printed; relay accepts with `["OK", <id>, true, ...]`.

#### 3b. Publish rejected: pubkey not whitelisted

**Tests:** An authenticated but non-whitelisted pubkey gets rejected.

```bash
nak key generate | xargs -I{} nak event -k 1 -c "I'm not allowed" --sec {} --auth ws://localhost:7777
```

**Expected:** `["OK", <id>, false, "restricted: pubkey not whitelisted"]`.

#### 3c. Publish rejected: content too long (kind 1)

**Tests:** Kind 1 events over 180 graphemes are rejected.

```bash
nak event -k 1 -c "$(python3 -c "print('a' * 181)")" --sec $ALICE_SEC --auth ws://localhost:7777
```

**Expected:** `["OK", <id>, false, "rejected: content exceeds 180 grapheme clusters"]`.

#### 3d. Publish a Kind 0 (metadata)

**Tests:** Metadata events are accepted and are replaceable (old one deleted on new publish).

```bash
nak event -k 0 \
  -c '{"name":"Alice","about":"Testing"}' \
  --sec $ALICE_SEC --auth ws://localhost:7777
```

Publish again with updated content. The old event should be replaced:

```bash
nak event -k 0 \
  -c '{"name":"Alice","about":"Updated profile"}' \
  --sec $ALICE_SEC --auth ws://localhost:7777
```

#### 3e. Query events (REQ)

**Tests:** Subscriptions return matching stored events, followed by EOSE. No auth needed for reads.

##### Fetch all kind 1 events

```bash
nak req -k 1 ws://localhost:7777
```

##### Fetch Alice's events

```bash
nak req -a $ALICE_PUB ws://localhost:7777
```

##### Fetch Alice's metadata (kind 0)

```bash
nak req -k 0 -a $ALICE_PUB ws://localhost:7777
```

**Expected:** Matching events printed as JSON, one per line.

#### 3f. Fetch with limit

```bash
nak req -k 1 -l 5 ws://localhost:7777
```

#### 3g. Stream live events

**Tests:** The `--stream` flag keeps the subscription open to receive new events in real-time.

Open a streaming subscription in one terminal:

```bash
nak req -k 1 --stream ws://localhost:7777
```

Then publish from another terminal:

```bash
nak event -k 1 -c "live event test" --sec $ALICE_SEC --auth ws://localhost:7777
```

**Expected:** The streaming terminal shows the new event as it arrives.

#### 3h. Publish rejected: unknown kind

**Tests:** Only kinds in `RELAY_ALLOWED_KINDS` are accepted (default: 0, 1, 2, 3, 4, 5, 6, 7, 16, 1111, 9735, 10000, 10001, 10002).

```bash
nak event -k 8 -c "kind 8 not allowed" --sec $ALICE_SEC --auth ws://localhost:7777
```

**Expected:** `["OK", <id>, false, "rejected: kind 8 not accepted"]`.

#### 3i. Publish rejected: future timestamp

**Tests:** Events with `created_at` more than 600 seconds in the future are rejected.

```bash
nak event -k 1 -c "from the future" \
  --ts $(($(date +%s) + 700)) \
  --sec $ALICE_SEC --auth ws://localhost:7777
```

**Expected:** `["OK", <id>, false, "rejected: created_at too far in the future"]`.

---

### 4. NIP-04: Encrypted Direct Messages

#### 4a. Encrypt and send a DM from Alice to Bob

**Tests:** Kind 4 events with NIP-04 encryption are accepted.

```bash
CIPHERTEXT=$(nak encrypt --nip04 --sec $ALICE_SEC -p $BOB_PUB "secret message from Alice")

nak event -k 4 \
  -c "$CIPHERTEXT" \
  -p $BOB_PUB \
  --sec $ALICE_SEC --auth ws://localhost:7777
```

**Expected:** Event accepted by relay.

#### 4b. Send a DM from Bob to Alice

```bash
CIPHERTEXT=$(nak encrypt --nip04 --sec $BOB_SEC -p $ALICE_PUB "secret reply from Bob")

nak event -k 4 \
  -c "$CIPHERTEXT" \
  -p $ALICE_PUB \
  --sec $BOB_SEC --auth ws://localhost:7777
```

#### 4c. Retrieve DMs sent to Bob

**Tests:** Querying kind 4 events tagged with Bob's pubkey returns his DMs.

```bash
nak req -k 4 -p $BOB_PUB ws://localhost:7777
```

#### 4d. Retrieve DMs authored by Alice

```bash
nak req -k 4 -a $ALICE_PUB ws://localhost:7777
```

#### 4e. Decrypt a received DM

Fetch the DM event and decrypt it. First get the event content:

```bash
EVENT_CONTENT=$(nak req -k 4 -p $BOB_PUB -l 1 ws://localhost:7777 | jq -r '.content')

nak decrypt --nip04 --sec $BOB_SEC -p $ALICE_PUB "$EVENT_CONTENT"
```

**Expected:** Outputs `secret message from Alice`.

#### 4f. Kind 4 rejected: content too large

**Tests:** Kind 4 content over 6144 bytes is rejected.

```bash
BIGTEXT=$(python3 -c "print('a' * 9000)")
CIPHERTEXT=$(nak encrypt --nip04 --sec $ALICE_SEC -p $BOB_PUB "$BIGTEXT")

nak event -k 4 \
  -c "$CIPHERTEXT" \
  -p $BOB_PUB \
  --sec $ALICE_SEC --auth ws://localhost:7777
```

**Expected:** `["OK", <id>, false, "rejected: kind 4 content exceeds 6144 bytes"]`.

---

### 5. Tag-Based Queries

#### 5a. Query by `#e` tag (event reference)

**Tests:** Filter by referenced event ID works.

```bash
# First publish an event, capture its ID
EVENT_ID=$(nak event -k 1 -c "original post" --sec $ALICE_SEC --auth ws://localhost:7777 | jq -r '.id')

# Publish a reply referencing it
nak event -k 1 -c "reply to original" -e $EVENT_ID --sec $BOB_SEC --auth ws://localhost:7777

# Query events referencing the original
nak req -e $EVENT_ID ws://localhost:7777
```

#### 5b. Query by `#p` tag (pubkey reference)

**Tests:** Filter by referenced pubkey works.

```bash
nak req -p $ALICE_PUB ws://localhost:7777
```

---

### 6. NIP-18: Reposts

#### 6a. Kind 6 repost

**Tests:** Publish a kind 1, then a kind 6 repost referencing it.

```bash
EVENT_ID=$(nak event -k 1 -c "original post" --sec $ALICE_SEC --auth ws://localhost:7777 | jq -r '.id')
nak event -k 6 -c "" -e $EVENT_ID --sec $ALICE_SEC --auth ws://localhost:7777
nak req -k 6 ws://localhost:7777
```

**Expected:** Kind 6 event stored and returned by query.

#### 6b. Kind 16 generic repost

```bash
nak event -k 16 -c "" -e $EVENT_ID --sec $ALICE_SEC --auth ws://localhost:7777
nak req -k 16 ws://localhost:7777
```

---

### 7. NIP-65: Relay List Metadata

#### 7a. Publish kind 10002 relay list

**Tests:** Relay list metadata is accepted and is replaceable.

```bash
nak event -k 10002 -c "" \
  -t r=wss://relay.example.com \
  -t r="wss://relay2.example.com;read" \
  --sec $ALICE_SEC --auth ws://localhost:7777

nak req -k 10002 -a $ALICE_PUB ws://localhost:7777
```

#### 7b. Replacement

Publish again — should replace the previous one.

```bash
nak event -k 10002 -c "" \
  -t r=wss://relay.newrelay.com \
  --sec $ALICE_SEC --auth ws://localhost:7777

nak req -k 10002 -a $ALICE_PUB ws://localhost:7777
```

**Expected:** Only the latest event returned.

---

### 8. NIP-51: Lists

#### 8a. Kind 10000 mute list (replaceable)

```bash
nak event -k 10000 -c "" \
  -p $BOB_PUB \
  --sec $ALICE_SEC --auth ws://localhost:7777

nak req -k 10000 -a $ALICE_PUB ws://localhost:7777
```

---

### 9. Edge Cases and Error Handling

#### 6a. Duplicate event

**Tests:** Publishing the same event twice returns OK but doesn't store a duplicate.

```bash
EVENT_JSON=$(nak event -k 1 -c "duplicate test" --sec $ALICE_SEC)

echo "$EVENT_JSON" | nak event --auth ws://localhost:7777
echo "$EVENT_JSON" | nak event --auth ws://localhost:7777
```

**Expected:** Both return OK. Querying should only return one copy.

#### 6b. Max subscriptions (20)

**Tests:** The relay enforces a maximum of 20 concurrent subscriptions per connection. This is harder to test with nak alone but can be verified by watching relay logs.

---

### Quick smoke test

Run these in order for a fast validation of all features:

```bash
source .env

ALICE_SEC=17fcb8fd6f58467365e9dc798347134240559ef0b32d31c492867895170db47d
ALICE_PUB=fcdff5dc286939a8a33dcc5a058181f7a74b41fb880bf9771f9a9b89d94c41c0
BOB_SEC=1e77483a4a1ff1202e0ce5d0eca12b6703475147e7ae949a0b759746cf09acfa
BOB_PUB=e59f6847ab9defc81a70d765f1a9dfe38ae6579eb97c5bbe929b173ff198b3a1

# 1. Whitelist both test keys
curl -s -X POST http://localhost:7777/admin/pubkey \
  -H "Authorization: Bearer $RELAY_ADMIN_KEY" \
  -d '{"pubkey":"'$ALICE_PUB'"}' | jq

curl -s -X POST http://localhost:7777/admin/pubkey \
  -H "Authorization: Bearer $RELAY_ADMIN_KEY" \
  -d '{"pubkey":"'$BOB_PUB'"}' | jq

# 2. NIP-11
nak relay ws://localhost:7777

# 3. Kind 0 - metadata (with auth)
nak event -k 0 -c '{"name":"Alice"}' --sec $ALICE_SEC --auth ws://localhost:7777

# 4. Kind 1 - short text (with auth)
nak event -k 1 -c "smoke test" --sec $ALICE_SEC --auth ws://localhost:7777

# 5. Kind 4 - encrypted DM (with auth)
CIPHERTEXT=$(nak encrypt --nip04 --sec $ALICE_SEC -p $BOB_PUB "hello Bob")
nak event -k 4 -c "$CIPHERTEXT" -p $BOB_PUB --sec $ALICE_SEC --auth ws://localhost:7777

# 6. Fetch all events (no auth needed for reads)
nak req ws://localhost:7777

# 7. Fetch Alice's events
nak req -a $ALICE_PUB ws://localhost:7777

# 8. Rejected - bad kind
nak event -k 8 -c "nope" --sec $ALICE_SEC --auth ws://localhost:7777

# 9. Rejected - not whitelisted
THROWAWAY=$(nak key generate)
nak event -k 1 -c "blocked" --sec $THROWAWAY --auth ws://localhost:7777
```
