# Bidirectional mesh sync, wire protocol 9

Unmanaged connections send and receive inventory events and attachments. Apps stay
editable while connected until a merge comparison needs review. Native nodes can listen, connect to several other nodes,
and relay received events to its other connections. Chains, stars, joined stars,
and cycles use the same protocol.

## Partition sync

The desktop starts in Partition sync by default. Use **Preferences → Sync** to start the usual listener, and
enter the listener's WebSocket base address in **Connect peer**. No voter
configuration is involved. A browser can connect with existing inventory and uses one desktop gateway at a time.

Each connected group elects one desktop. After a topology change, edits queue
in memory while the link information settles. The leader checks a proposal
against approved history before committing it. A rejected edit is shown to its
author in **Sync**. The history and the inventory advance only on approval.
A proposal retains its ID and contents across retries. Pending in-memory
proposals are cleared by quitting the application.

A browser with no active desktop connection saves edits directly to its local
SQLite database and action chunks. This includes edits made after an earlier
sync session. When the browser connects again, its user reviews any overlaps
with the desktop history before publishing the combined changes. Automatic
connection retries are held after offline edits until the user reconnects.

A disconnected group, including a single desktop, elects its own leader and
continues accepting edits. An approval is durable on the leader once written.
If that leader is lost before it forwards an approval, peers may have to wait
for its history to return. Election converges as topology updates propagate;
a delayed or broken link can produce independent approved histories.

### Reconnect two branches

Enter a peer address in Sync to compare the two histories. The person initiating the connection reviews any overlaps on their desktop or browser while the other group continues to save. A connection with no conflicts proceeds automatically. Once the comparisons are
resolved, choose **Apply merge and connect**. Both leaders check their
history again before publishing the merge. If either branch advanced, use
**Refresh comparison** and review new overlaps. The new link opens after
the merge reaches both endpoints, and the joined group elects one leader.
If the connection fails during publication, refresh the comparison and retry;
already imported operations retain their original IDs.

The reconnect exchange currently requires the approved histories to fit in one
WebSocket message. Attachments travel separately.

Former Raft workspaces open in Partition sync. The desktop imports any
committed records still present in the old consensus database. Start the
usual Sync listener in **Preferences → Sync**. Requests to `/raft`
receive an error directing clients to the Sync base address.

## Connecting unmanaged apps

Configure the listener in **Preferences → Network → Sync**. The default
`0.0.0.0:8824` accepts peers on all IPv4 interfaces at `/sync`. Use the node’s
discovery hostname for LAN connections, or set `127.0.0.1:8824` to accept only
local connections. To connect by LAN IP with a browser-trusted automatic
certificate, bind to that specific IP. IPv6 addresses also work.

Enter `wss://PEER-HOSTNAME:8824` and choose **Connect peer**. Multiple outgoing peers
can be added; an app can also listen at the same time. Listening and connecting
are transport roles only: every connection carries updates in both directions.
You can connect existing independently populated inventories; a new empty database
is no longer required. The window shows this app's ID, connected outgoing peer IDs,
and merge conflicts that need review. Source/receiver read-only mode is removed.

Disconnect/reconnect explicitly after a failed connection to resume. Connection
settings are not persisted or automatically started at launch. Stopping a listener
closes its inbound sessions; other connections continue working.

## Browser app

The WASM app's header **Sync** control opens outgoing WebSocket connections to
native listeners. Every connection is bidirectional, with live changes and separate
image/PDF transfer. Multiple outgoing peers can be connected and events relayed
between them. Browsers cannot host the listening socket. The browser uses SQLite
and the shared Rust replay/merge engine; comparisons and editable Markdown merge
drafts can be resolved directly in its Sync window. See [browser storage](browser-storage.md)
for persistence, permissions, secure-page requirements and verification.

## Network status and topology gossip

The node count at the bottom left opens a network diagram in native
and browser builds. Each node has a computer icon and a short title (Desktop or
Browser plus the first eight characters of its application ID). Browser nodes
carry a WebAssembly badge, and a green dot marks this app. Unreachable nodes
appear in grayscale. Hover for the full identity, drag to pan, scroll to zoom,
or choose **Fit**. Links represent connections, regardless of which
end initiated the socket. Multiple sockets between the same two IDs form one link.

The Hello message adds a `topology` array. Each advertisement has `id`, `title`,
`epoch`, `sequence`, and `neighbors` (directly connected application IDs). Subsequent
text messages use `{"type":"topology","advertisements":[...]}`. Every application
announces its own neighbors and relays newer announcements from other applications.
This is link-state gossip rather than a growing list of retransmission hops: A can
learn B–C without incorrectly drawing A–C, and cycles do not grow or repeatedly
circulate the same announcement. Each connection sends an origin/version once.

The SQLite `sync_topology_epoch` launch counter and per-run sequence order updates,
including restarts. Duplicate versions do not refresh expiry. Updates are sent
when a connection is established or closed; fresh announcements occur every 20
seconds. Unrefreshed announcements expire after 75 seconds measured on each
receiver's monotonic clock. A link is drawn only when both endpoints advertise it,
so a disconnect removes it immediately on receipt even if the other endpoint
crashed. Unreachable nodes may remain gray until expiry. Direct peers are shown
while their initial announcement is still arriving. Maps cover the reachable gossip
network; disconnected partitions cannot report changes to one another.

Topology is ephemeral connection information, separate from inventory events,
HEADs and action chunks. The map is rebuilt after launch. Bounds are 512 known
nodes and 64 neighbors per announcement. All peers must use protocol 7.

## Local KiCad data

KiCad-derived Requested rows and the project file binding are local to each app.
They are excluded from initial exports, action records and new mesh mutations.
Opening the same KiCad project locally reconstructs its requests. Local SQLite
retains the binding and request cache across reloads, remote edits and project
renames. A fresh database rebuilt from chunks needs the KiCad file opened again.
Manual requests, project identity/description/notes, real allocations, stock and
purchases still synchronize. Existing historical chunks are not rewritten; their
old KiCad payloads are ignored by the new projection rather than becoming local
bindings or requests on a new receiver.

## Durable identities and progress

On first open the app generates a signing key and stores its derived node ID in
`sync_identity`. Reopening derives the same ID from the stored key. Each local event has a
strictly increasing sequence within that ID. SQLite `sync_events` stores the
original event ID, origin application ID, sequence, logical time, UTC timestamp,
and payload. `(origin, sequence)` and event ID are unique.

`sync_peers` stores the ID of each direct peer, its last received-message time in
Unix milliseconds, and this app's received progress at that time. Origin progress
is reconstructed from persisted events, including gaps and causal dependencies.
An event's origin stays unchanged when another app relays it.

A copied SQLite database also copies its identity. Use an empty database and sync
to create another independent app identity. Same-ID connections and reused origin
sequences with different events are rejected, rather than silently combining
cloned writers.

Progress is a vector clock: `{ "ORIGIN-ID": last_contiguous_causally_available_sequence }`.
UTC dates are retained for history and audit; they are not used as sync cursors.
Sequences avoid skipping two changes with the same timestamp or missing changes
when computer clocks disagree. A peer sends only events beyond the progress
advertised by the other app, plus any still-needed origins it learned by relaying.

## Handshake and transport

Both ends immediately send this JSON text message, then validate the other's:

```json
{"type":"hello","version":9,"application_id":"0123456789abcdef01234567","clock":{}}
```

The node ID must match the public-key-derived identity authenticated before this hello.
Both apps must run protocol 9. Older unsigned protocols cannot join. The app adds
`/sync` to the WSS base address. See secure transport and node membership below.

Each binary event message uses the existing frame codec:

1. Four-byte unsigned little-endian compressed JSON length.
2. One gzip member containing an envelope:

```json
{
  "kind": "event",
  "record": {
    "version": 2,
    "id": "EVENT-UUID",
    "timestamp_ms": 1793830000000,
    "initial": false,
    "actions": [],
    "mesh": {
      "origin": "ORIGIN-ID",
      "sequence": 1,
      "lamport": 1,
      "context": {},
      "mutations": [],
      "receipts": []
    }
  }
}
```

`kind` distinguishes events from managed save proposals. `context` contains the contiguous origin progress observed
by the creator. An event's own-origin context must be `sequence - 1`. Logical time
is greater than every observed predecessor, independent of wall time.

After its initial outgoing backlog and current-file repair, an app sends:

```json
{"type":"ready","version":9,"clock":{"ORIGIN-ID":5}}
```

After saving a received event it sends:

```json
{"type":"ack","id":"EVENT-UUID","applied":true,"clock":{"ORIGIN-ID":5}}
```

Here `applied: true` means newly accepted and durably stored. A causally pending
event may still await predecessors before changing the visible inventory.
An exact duplicate is acknowledged with `applied: false`, without appending it
again, changing stock, or triggering another broadcast. Reusing an ID or an
origin sequence for different content is an error. Acks may interleave with
outgoing events, files, and WebSocket ping/pong frames.

Reader and writer processing run concurrently with a bounded send queue, so both
apps can send initial histories or large files without waiting for the other side
to finish first. Committed-event notifications wake every connection. Per-session
sent-ID tracking and durable received-event deduplication stop cycles from becoming
endless rebroadcasts. A received event is forwarded with its original origin,
sequence, timestamp, payload, and event ID, never as a synthetic local edit.

Event JSON is bounded to 64 MiB compressed and expanded. Messages allow additional
blob-header overhead. Handshakes, stalled writes, and unresponsive peers have
timeouts; pings are sent every twenty seconds. Each listener admits at most 32 unmanaged
simultaneous inbound connections; larger networks can span additional nodes.

## Deterministic merge rules

The inventory is a projection of its durable event set. Causally ready events are
processed in `(Lamport time, origin ID, origin sequence, event ID)` order. All apps
with the same event set therefore produce the same inventory, independent of
arrival order or route. Events with missing predecessors are saved, held pending,
and do not advance the contiguous cursor until those predecessors arrive.

A mutation contains the entity kind, stable identity, observed member IDs,
complete before/after values, and source collection position. This records intent
without interpreting a remote full replacement as a new absolute stock total.

- **Parts:** group by trimmed, case-insensitive part number. Distinct part UUIDs
  contribute their stock once; subsequent edits contribute signed quantity
  differences. Shared UUID baselines and replayed events are not counted twice.
  A deterministic canonical UUID represents the group. Allocation and BOM part
  references are remapped from all member UUIDs. Other printed barcodes remain
  searchable through `syncBarcodeAliases`.
- **Purchases:** use the same grouping and stock-counter rules, separately from
  inventory parts. Assignment IDs are retained and combined. Receiving a purchase
  records claims against its original member IDs. Concurrently receiving the same
  shared purchase does not create inventory stock twice; independently created
  purchases still contribute independently.
- **Other fields:** the projection has a deterministic logical order, but protocol 7
  stages conflicting concurrent field edits for explicit review (see below). An
  unrelated stock edit does not overwrite a newer notes or description edit.
  Nested values such as a notes document or custom-fields map are single fields.
  Concurrent changes to the same field retain both edits until a resolution is
  committed.
- **Projects:** newly created projects have UUIDs. Existing projects receive stable
  migrated IDs. Mesh relationships use IDs, so renames update all references.
  Different project IDs with the same name remain separate; a deterministic ID
  suffix makes their displayed/database names unique. This is reported as a
  conflict in Sync.
- **Deletions:** remove the observed member IDs. A new independently created member
  is not erased by an offline deletion that never observed it. A later logical
  edit can restore an observed member; equal-time conflicts use origin ID order.
- **Allocations and assignments:** concurrent requests may exceed available stock.
  Stable event/collection order determines accepted quantities, capped at the
  available quantity. The original requests remain in events, and Sync reports
  every capped request. Increasing stock can restore those requests without
  inventing stock or losing their original quantities.
- **Stock overdraw:** concurrent decrements may produce a negative underlying
  counter. The app displays zero, reports the deficit, and retains it in the
  counter; future incoming stock first repays that deficit. Counts above the
  SQLite quantity range are likewise capped and reported while their full
  underlying counters remain in the event projection. A purchase whose concurrent
  quantity edits exhaust its stock is hidden from the active cart and reported;
  its underlying contributions remain available when stock is added again.

The merged inventory is validated before committing it. Missing project/part
relationships are retained in event history and restored when their entities
become available. BOM quantities, reservations, and designator precedence are
recomputed from the same deterministic result on each app.

## Files travel before their events

Blob messages remain separate from event chunks:

1. Eight ASCII bytes `BOMBLB01`.
2. Four-byte little-endian JSON-header length, at most 4096 bytes.
3. Header: `{"asset":"asset:<hex>","size":123,"crc32":123456789}`.
4. Exactly `size` raw bytes, at most 64 MiB per file.

The sender collects asset references from action replacements and mesh mutations.
Original/preview/thumbnail images, PDFs, and part/purchase/project note images are
sent before the event. References shared by several records are sent once per
connection. A relay sends the same files before forwarding the event further.

The receiver checks length, CRC32, and the relative path, creates directories,
writes to a temporary file, flushes it, publishes atomically, and flushes the parent
directory. A network event with a missing referenced file is rejected before its
WAL is written. The exact attachment-relative path is retained under the receiver's
own `Attachments/` root. Identical existing files are reused; conflicting bytes
at the same path are rejected. New file contents must use new UUID paths.
Traversal, absolute paths, NULs, and symlink files/directories are rejected.

On reconnect, each peer also sends available current attachments to repair missing
files without replaying old events. A missing local file in this optional repair
pass is skipped so another peer can supply it; missing files required for a new
outgoing event stop that transfer. External URLs and unmanaged paths are not
fetched. Blob bytes never enter the action log or event-index payloads. Removing a
reference does not physically delete historical files. Unreferenced completed
uploads can remain after a rejected event; garbage collection is separate.

## Persistence and migration

For a received event, required blobs are saved first, then the event frame is
appended and flushed to the local chunk. The materialized inventory, applied marker,
and event index commit together in SQLite. Only then are other connections
notified. A SQLite failure after logging blocks further writes until startup
recovery; retrying that same event after recovery remains safe.

Chunk framing (`BOMLOG01`, length + gzip JSON) is unchanged. New mesh records use
record version 2; readers continue accepting legacy record version 1. During
upgrade, legacy records become deterministic events keyed by their existing record
UUIDs, using those UUIDs as virtual origins. Thus two apps with shared old history
do not import that shared stock twice. Legacy project identities derive from their
retained notes-owner identity. Subsequent local events use the new persisted app ID.

Startup rebuilds the projection from durable events and migrates existing chunks.
SQLite's committed event index can repair a missing chunk directory without
resetting origin counters. A genuinely old installation with neither chunks nor an
event index gets the initial inventory export. A fresh database can reconstruct
inventory from complete chunks and their separate attachment tree.

Conflict details are deterministic and recomputed on restart. This version retains
history in memory and recomputes the projection after each change; compaction and
incremental projection remain future performance work. Authentication, TLS,
automatic connection retry, and historical file garbage collection are also not
implemented. Connection loss is shown in Sync; reconnecting catches up both ways.

## Pull–resolve–publish workflow (wire protocol 9)

Unmanaged workspaces use explicit conflict reviews. Both ends
exchange a content-addressed history HEAD and its latest event date in Hello,
alongside app IDs and sparse per-origin cursors. The hash fingerprints canonical
history content; dates remain informational. Cursor ranges still fetch only missing
events, including divergent histories that do not share a single linear tip.

An outgoing connection pulls first: its local-origin publication is held until the
peer finishes its initial backlog. Other existing connections also respect this
hold. When concurrent changes edit the same field differently, the receiving app
keeps its currently visible inventory, saves incoming events durably, and holds the
conflicting branch and causal descendants from further publication. Normal edits
pause during review to protect that snapshot. Non-conflicting fields and independent
part/purchase additions retain their merge behavior. Identical concurrent values
need no review.

Open Sync for **Common version / Ours / Theirs**, **Use ours**, **Use theirs**, an
editable result, and **Resolve**. Text fields use a plain-text result; quantities and structured fields use an
editable JSON value. Markdown notes use a three-way line merge, with editable conflict markers for
overlapping edits and a text preview. Disjoint Markdown edits merge automatically;
conflicting note image mappings require review. Markdown drafts are stored locally
in SQLite and survive restart. Conflict markers must be removed before Resolve.

A resolution is a new durable event carrying the conflict ID, selected value, causal
context, and both branch HEAD hashes as parents. Receiving it reuses the decision
rather than asking the same question again. Once every pending comparison is resolved,
the resulting branch is applied and publication resumes. Sync shows shared/publishable
and local/received HEAD hashes. Equal final event sets yield equal hashes.

Concurrent incompatible resolutions can themselves require another comparison.
The quantity/relationship warnings described earlier remain separate invariant
warnings: independent reservations that together exceed stock still need inventory
review, even when they did not edit the same field. This is an event-DAG workflow,
not a Git repository or a globally elected blockchain tip; temporary branch heads
exist until merges and their decisions reach all connected peers.

All participating apps must use protocol **7**. Chunk framing remains unchanged;
record version 2 now includes optional resolution and parent metadata. Older chunks
continue to load. Unsaved merge decisions are not silently chosen on restart.

## Secure transport and node membership (protocol 9)

Protocol 9 requires WSS for Sync, branch review, and Git-bundle transfer. It is
incompatible with UUID-based peers and their signed history. Use fresh
inventories and action logs when switching to this version. The extension endpoint remains restricted to loopback
connections with a Chrome-extension Origin and now also uses WSS.

Each inventory instance persists a P-256 private key locally in SQLite; it is
never included in inventory synchronization. The node ID is the first 12 bytes of the public-key SHA-256 fingerprint,
encoded as 24 lowercase hexadecimal characters. Browser key generation, signing, verification,
and membership evaluation run in Rust/WASM; JavaScript handles WebSocket and
storage access. A TLS connection does not itself grant inventory access.

Before sending inventory hello, topology, files, proposals, or bundle grants,
both peers exchange random challenges and sign a domain-separated transcript
containing both IDs and both nonces. The peer must prove possession of its key.
For a first desktop connection, open **Sync → Discover** on the host and
click **Approve** on the pending node to reveal its four-digit code. Enter it
in the connecting app’s Sync dialog. This exchanges only pairing messages
until both apps verify their signed identities and the client confirms the code.
The code is derived from the TLS session, expires after two minutes, and allows
three attempts. Each desktop remembers the other’s TLS public key locally, and
the automatic listener keeps that key across certificate renewals. If the host
key changes, the client stops connecting until you explicitly pair again.

Browser connections continue to use browser certificate validation and the
signed membership flow. Unknown browser peers wait for approval in Discover.
Existing members may sponsor additional public keys; signed approvals propagate
through the connected mesh, so already authorized peers do not need repeated
manual approvals. Independent inventories can explicitly approve each other;
there is no global administrator/root service in this initial version.

The **signed network action log** is persisted independently from inventory
edits (`network_actions` on desktop, `network_ledger` in browser SQLite) and
replicated on authenticated connections. It records approvals and permanent
revocations. Any currently authorized member can issue either. Revocation
closes affected sync connections and invalidates bundle grants. Rejoining after
revocation requires a new key. Existing accepted records are retained by exact
signed record hashes in the revocation, rather than trusting an author-supplied
timestamp. Inventory action signatures cover the complete record, including
mesh metadata, and remain intact through relays. Unapproved authors, modified
records, and unsigned records are rejected.

### Local HTTPS/WSS testing

Generate local development credentials (does not install trust):

```sh
python3 script/generate_test_certificates.py
# To include a LAN address, generate into a fresh directory with --host 192.168.x.y.
```

The default output is ignored by Git: `.local-tls/ca.pem`, `server.pem`, and
`server-key.pem`. Trust **ca.pem** manually in the OS/browser certificate store;
never distribute either private key. The CA is valid for 90 days and its server
certificate for 30 days. Server SANs include localhost, loopback addresses, and
the hostname/addresses discovered at generation; use `--host` for other names.
Changing a host address requires a certificate valid for that address.

The desktop listener starts on `0.0.0.0:8824` when no listener preference
has been saved. It generates a private local CA and issues a fresh server
certificate on each listener start. The certificate covers the node’s discovery
hostname, localhost, loopback addresses, and any specific LAN IP used for the bind.

In **Preferences → NETWORK → Sync**, use **Export CA certificate…** and trust
the exported certificate on each connecting device. The CA signing key stays in
the inventory folder. To use your own certificate, save its PEM and key paths
and restart the listener. Existing saved paths and the
`VINVENTORY_TLS_CERT`/`VINVENTORY_TLS_KEY` environment overrides are supported.

`script/web.sh` serves **https://localhost:8787** using the test certificate
described above. The browser page and the desktop WSS listener can use separate
CAs. Trust each issuer whose endpoints the browser accesses. The Python server
also accepts `--cert`/`--key`; `--http` is for isolated localhost tests.

If partition histories diverge after pairing, use **Review branches** in browser
Sync (desktop uses its existing reconnect review). Review endpoints require the
same approved-key authentication as normal sync.

### Nearby nodes

Desktop instances discover `_vispace._tcp.local.` services on the local network.
Sync shows the short node ID and a Connect button. Approved peers can connect
automatically (controlled by the checkbox in Sync). A discovery record does not
grant access: the peer must still prove ownership of its key and pass the
membership checks. The hostname uses the same ID as the signed sync protocol.

Discover keeps approved peers visible while offline. Unapproved peers disappear
after 24 hours without a new sighting or active pairing. Revoked peers disappear
24 hours after revocation. Their signed revocations stay in the network action log
and continue to block the keys.

A desktop listener advertises only when it is bound to a LAN address (or
`0.0.0.0`) and its TLS certificate covers `<node-id>.vispace.local`. To issue a
test certificate using your existing development CA, copy the short ID from
Sync and run from the workspace root:

```sh
python3 script/generate_test_certificates.py \
  --ca-directory .local-tls \
  --directory .local-tls/nearby \
  --node-id <node-id>
```

Select the new server certificate and key in Preferences → Sync and restart the
listener. Connecting devices must trust the issuing CA. Use an address covered
by the certificate when connecting manually.

Browser discovery uses the optional Chrome extension native helper. If the
helper is unavailable, enter a WSS address in Sync. The helper only discovers
addresses; Chrome still needs to resolve the selected name, trust its TLS
certificate, and allow access to the local network.
