# Browser SQLite and inventory workspace

The browser app uses the official SQLite 3.54.1 WASM distribution in a dedicated
module worker. Its OPFS SyncAccessHandle pool VFS keeps the working database in
browser-managed persistent storage. The UI thread never operates SQLite directly.
The native SQLite/HTTP server remains separate.

## Locations and controls

| Data | Location |
| --- | --- |
| Working database, app UUID, canonical inventory, event index | SQLite in OPFS, `.bom-inventory-sqlite` |
| Internal files and journal mirror | OPFS `bom-inventory-workspace` |
| Folder bookmark | IndexedDB `folders` |
| Small UI preferences and unrelated KiCad caches | Existing browser adapters |
| Portable workspace | User-selected directory, when supported |

**Preferences → General → Choose folder…** requests read/write access during the user's click. The selected
workspace has this layout:

```text
Workspace/
  Attachments/browser/<content-sha256>.png
  Attachments/browser/<content-sha256>.pdf
  inventory.actions/<timestamp>-<uuid>.wal
  backups/inventory-<timestamp>.sqlite
```

Imported native assets keep their existing attachment-relative paths. Browser data
URLs become separate immutable files, deduplicated by content hash. The UI can still
use data URLs to render images, while SQLite and action payloads store `asset:<hex>`
references. Those references use the native protocol's relative-path representation.
The OPFS file mirror allows normal reloads and browser-owned mode without repeatedly
asking for directory access.

The live SQLite database stays in OPFS. SQLite recovery snapshots include core
native-format tables, app identity, and event history. Keep the attachment files
alongside a recovery snapshot when restoring.

## Saves, permission loss, and concurrency

A save finishes these steps before its promise resolves:

1. Check selected-directory write permission, without prompting in background.
2. Extract new media bytes and finish immutable file writes.
3. Append and close a framed gzip action record in the selected workspace, then
   the private mirror. A new session opens a new chunk; rotation is 8 MiB.
4. Commit canonical inventory tables and the event index in one SQLite transaction.
5. Return the resulting inventory snapshot to the Rust UI.

The shared Rust engine generates and replays records; the browser does not invent a
second action schema. Event files retain `BOMLOG01` framing and existing record
versions. Signed quantity changes, stable IDs, deduplication, and replay use the
same core as native code. Dates use the browser clock in WASM.

The current synchronous egui mutation interface displays the first change while
its asynchronous save is pending. Additional writes are rejected until that save
finishes. A failure blocks subsequent saves, preserves the in-memory state for
retry, and warns before leaving the page. Supplier confirmations await the durable
save promise. The UI is updated from the committed projection after a successful
save, so subsequent edits use canonical IDs and quantities.

When directory permission is missing, existing database data can be read, but edits
are paused. Use **Choose folder…** in General and select the same workspace.
This requests permission from a user gesture, replays
available completed logs, retries an unsaved edit when possible, and reloads the
app after a successful bind. It never silently switches to browser-only writes
while a selected workspace is disconnected.

One worker holds a Web Lock for the database lifetime. Another tab is rejected with
a message to close the existing tab. The SQLite VFS also owns its OPFS access
handles. Stale revision checks protect the worker interface independently of that
lock. Directory bookmarks are remembered, but permission is checked again on later
visits.

## Migration, opening workspaces, and recovery

Existing IndexedDB/localStorage inventory snapshots are read only as migration
input when no SQLite state exists. Their original copies are retained. The worker
extracts media, writes an initial replayable record, and commits SQLite before the
new state is acknowledged. Even an empty installation creates an initial record.

At startup the worker combines its committed event index with complete records in
private and authorized workspace chunks. A record flushed before a failed SQLite
commit is therefore replayed on reopening. Incomplete trailing frames are ignored;
corrupt complete frames fail explicitly. The committed index can repair missing
private chunks. A fresh working database can be rebuilt from the workspace chunks
without adding stock twice.

Selecting an existing workspace validates its history, imports compatible events,
and copies missing attachments/chunks between the private mirror and selected
folder. Existing different bytes at the same path are an error; files are never
silently overwritten. The app reloads after binding so Rust and the page share the
same committed snapshot.

Workspaces and incoming network histories may contain unresolved field conflicts.
Those events are durable, but the visible inventory stays at its previous committed
snapshot until the comparisons are resolved in **Sync**. Local editing is paused
while comparisons remain. Do not use the selected directory as a live substitute
for WebSocket sync while simultaneously editing it in another application.

## Browser mesh connections

Open **Sync** in the bottom bar, choose **Connect**, and enter the native app's WebSocket address
(for example `wss://192.168.1.20:8824`), and choose **Connect**. Start listening
in that native app first. A browser opens outgoing connections; it cannot listen
for incoming TCP/WebSocket connections. Each connection transfers changes in both
directions. Several outgoing connections let a browser relay events between peers.

The worker uses protocol 7, the same framed gzip records and blob envelope as the
native app. The identity, received history and peer progress are persisted in
SQLite. The handshake advertises identity, HEAD and causal progress. The browser
receives the peer's initial backlog and Ready message before publishing its local
changes. Events keep their original IDs when relayed. Acknowledgements are sent
only after required files, the action chunk and SQLite commit are complete.
Disconnect and connect again after a failed connection; addresses are not restored
automatically on reload. An HTTPS page requires a `wss://` endpoint supplied through
a TLS proxy; the native listener itself uses `ws://`.

The Sync window shows local and peer HEADs and pending comparisons with the common,
our and their versions. Choose **Use ours**, **Use theirs**, or edit the proposed
value, then **Resolve**. Notes provide a three-way Markdown merge draft, editable
source and a text preview. Overlapping lines have conflict markers that must be
removed before resolving. Other fields use editable JSON. Drafts are saved to SQLite
while typing. The final choice becomes a normal durable merge event and propagates
to other peers. Browser and native merge decisions use the same Rust code.

New image/PDF bytes are sent first and saved under exactly the same safe relative
attachment path on the receiver. Byte-identical retries reuse files; different
bytes at an existing path are rejected. Files remain separate from the history.

OPFS and Web Locks require a supporting secure browser context (HTTPS or localhost).
Directory selection additionally requires `showDirectoryPicker`; unsupported
browsers keep files and chunks in OPFS and can download exports. Site-data clearing
or moving to a different origin can remove access to browser-owned storage. A
selected external workspace and explicit backups provide recovery outside that
browser storage. Filesystem directory layouts are portable, but an OPFS working
database is not itself a user-visible filesystem file.

## Verification

Build the browser artifacts:

```sh
./script/web.sh --build-only
```

Run host/RPC permission tests with a current Node runtime:

```sh
node --test web/host.test.mjs web/storage-client.test.mjs web/sync-client.test.mjs
```

Run the real-browser integration test:

```sh
python3 script/test_browser_storage.py
```

It needs Google Chrome and Python `websocket-client`. It creates a temporary Chrome
profile and a disposable localhost server, leaving the user's browser untouched.
The test page refuses to run on an origin already containing an inventory database.
It verifies migration, identities, single-writer locking, revisions, separate file
bytes, replayable chunks, permission decisions, reload, recovery after deleting the
working database, native SQLite export, and saves through the actual WASM UI bridge.
Its simulated selected directory is an OPFS directory handle exercising the same
file methods; OS directory-picker interaction is a manual browser check.

For real browser/native mesh checks, build the test peer and run:

```sh
cargo build --example browser_sync_peer
python3 script/test_browser_sync.py
```

This uses two temporary native peers and an isolated browser profile. It checks
live edits, relaying through the browser, separate image/PDF transfer in both
directions, comparison drafts and resolution through the window, matching HEADs,
and persisted identity/progress after reload.

The node count in the app's bottom bar opens the mesh diagram, including
peers learned through relays. Topology announcements are not inventory actions and
do not affect HEADs. KiCad links and generated requested rows likewise stay out of
new chunks and sync. Their local SQLite cache survives normal reloads; recovering
from chunks alone requires opening the local KiCad file again.
