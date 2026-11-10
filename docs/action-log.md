# Inventory action protocol and chunk format

The native Rust application keeps an append-only write-ahead action log beside its
SQLite database: `inventory.sqlite` uses `inventory.actions/`. This is the
durable history used by the native [WebSocket sync service](websocket-sync.md).
The browser uses the same shared action engine and chunk format with SQLite in OPFS;
see [browser storage](browser-storage.md).

## Inventory history

Click **HEAD** in the footer to open Inventory History with the newest 30 operations. Scrolling to the bottom loads the next
30 older operations, continuing until the beginning of the journal. Pages read
only the needed record payloads. A chunk/end-offset cursor keeps older pages stable
when new actions arrive; **Refresh** returns to the newest page.

Each operation displays the first ten hexadecimal characters of its SHA-1 content
hash. Hover to see the full hash, event UUID and chunk name. This fingerprints the
individual serialized action record. The footer HEAD shows the last operation in
the deterministic replay order committed to SQLite. It stays fixed while incoming
events are staged for review. The sync protocol also fingerprints event sets
separately. Hashes do not change the wire or chunk format.

## Wire and disk format

A chunk is named `<20-digit-monotonic-time>-<UUID>.wal`. Names sort in creation
order even if the clock moves backwards or several chunks start in one millisecond.
Each file starts with the eight ASCII bytes `BOMLOG01`. It then contains frames:

1. Four-byte unsigned little-endian compressed payload length.
2. One gzip member containing UTF-8 JSON for one atomic operation.

Gzip supplies compression and CRC integrity checking. This binary framing with
compressed JSON was chosen over bincode: existing models contain flattened,
extensible JSON fields, and named fields make protocol evolution and inspection
simpler. WebSocket sync uses the same frame codec around a transfer envelope. Neither
compressed nor expanded payload may exceed 64 MiB. A single operation is never
split across files; the normal rotation threshold is 8 MiB, so an unusually large
operation may exceed that threshold. A new writer chunk opens after startup recovery in each application session. Closing requires no footer: every accepted
operation has already been flushed with `sync_all`.

A record contains:

| Field | Meaning |
| --- | --- |
| `version` | `1` for legacy snapshots, `2` for mesh events; unknown versions fail recovery. |
| `id` | UUID of this operation; used for durable duplicate suppression. |
| `timestamp_ms` | UTC Unix milliseconds for display, not replay ordering. |
| `initial` | True for the initial inventory export. |
| `actions` | Entity replacements/deletions describing one committed operation. |
| `mesh` | Present on version 2: origin ID, sequence, logical time, causal context, mutations and receipt claims. |

Each action has `kind`, `entity`, `key`, `value`, `changes`, `label`, and
`position`. `label` identifies the affected item in history, including after
deletion. `value` is the
complete resulting entity (or `null` for deletion). Replacements avoid depending
on the receiver's UI defaults or repeating commands such as “increment stock”.
`position` preserves collection order (including BOM allocation budgeting); it is
null for storage entries and deletions. A collection move emits an update even
when its entity payload is unchanged. Legacy actions in a record are applied before
validating the resulting inventory;
relationships can therefore be created or removed in the same operation.

## Actions

The following are authoritative entity actions. Every entity supports `_added`,
`_updated`, and `_removed`. Add/update are upserts; remove deletes the identity.

| Kinds | Entity / identity | Payload and behavior |
| --- | --- | --- |
| `part_added`, `part_updated`, `part_removed` | `parts` / part UUID | Full part, including stock, barcode, supplier, labels, custom fields, notes, storage assignment, and asset references. |
| `project_added`, `project_updated`, `project_removed` | `projects` / project name | Description, notes, and stable notes attachment owner. KiCad bindings stay local. Rename is a remove/add with related allocation, BOM, and purchase changes in the same record. |
| `storage_added`, `storage_updated`, `storage_removed` | `storage` / location name | Description string. Renames and removals include affected part replacements. |
| `allocation_added`, `allocation_updated`, `allocation_removed` | `allocations` / allocation UUID | Part UUID, project name, quantity, and designators. Covers project assignment, amount edits, and unassignment. |
| `purchase_added`, `purchase_updated`, `purchase_removed` | `purchases` / purchase part UUID | Full purchase part and assignments, each with UUID, project, quantity, and designators. Receiving stock or merging purchases records every resulting entity change atomically. |
| `bom_entry_added`, `bom_entry_updated`, `bom_entry_removed` | `bom` / entry UUID | Manually added requested part details, quantity, designators, project and optional matched part/allocation. KiCad-derived rows are excluded. |

`changes` supplies descriptive sub-actions for history, carried alongside the full
replacement rather than applied a second time:

| Sub-action | Meaning |
| --- | --- |
| `notes_added`, `notes_updated`, `notes_removed` | Notes created, edited, or cleared for a part, purchase, or project. Includes note image references. |
| `image_added`, `image_updated`, `image_removed` | Part/purchase image attached, replaced, or detached, whether downloaded, pasted, or imported. No image bytes. |
| `datasheet_added`, `datasheet_updated`, `datasheet_removed` | PDF reference or external URL attached, replaced, or cleared. No PDF bytes. |
| `storage_assignment_added`, `storage_assignment_updated`, `storage_assignment_removed` | Part/purchase storage field assigned, changed, or cleared. |
| `stock_added`, `stock_updated`, `stock_removed` | Stock field introduced, changed, or owner removed. Quantities live in the replacement payload. |
| `purchase_assignments_changed` | Purchase assignments added, changed, or removed; replacement contains the complete resulting list. |

A no-op or failed validation writes no action. Imports, bulk operations, UI edits,
and HTTP API mutations all use the same store commit boundary. Temporary downloads
that are cancelled are not inventory operations and do not enter the log. Labels
and suggestion tables are derived by the store from inventory and are rebuilt.
Settings, printer configuration, cached rendering, and backup credentials are local
application state and are outside this inventory protocol.

## Assets and portability

Managed paths under the database's `Attachments/` directory become `asset:<hex>`
references. The hex is the UTF-8 attachment-relative path; it is a unique,
root-independent identifier, preserving the app's existing UUID filenames and
folders. The receiver resolves it under its own `Attachments/` directory. Unsafe
paths (absolute or containing parent traversal) are rejected. Literal text starting
with `asset:` or `literal:` is escaped with `literal:` to avoid ambiguity.

No image or PDF bytes are read into the log. Embedded image/PDF data URIs are
rejected. Removing a reference removes it from the inventory; it does not delete
physical files, which may still be referenced elsewhere or by historical records.
WebSocket sync transfers these files as separate blob messages using the same
asset identifiers. Garbage collection remains separate.
For offline reconstruction of both data and files, copy the chunks and the corresponding
`Attachments/` tree separately. Replay works with missing files and restores their
expected references; it cannot reconstruct missing asset bytes. External URLs and
KiCad project paths, folder bookmarks and project-file snapshots remain local and
are excluded from new records.

## Startup, durability, and recovery

Version-2 records are merged by the deterministic mesh projection, not by replaying
their action replacements in arbitrary network order. Their before/after mutations
produce quantity deltas and carry stable relationship IDs. Legacy version-1 records
are migrated once, preserving their original record UUIDs for duplicate suppression.

The app locks `writer.lock` with an OS file lock for the lifetime of the store;
concurrent writers to the same log directory are rejected and crashes release the
lock automatically. Do not delete the lock file. Startup reads chunks in filename
order and frames in byte order. `action_log_applied` in SQLite stores each record
UUID, chunk name, ending byte offset, and timestamp. Applied markers and inventory
changes commit in the same SQLite transaction. Dates alone are insufficient to
track crash recovery and are deliberately not used as cursors.

For a new operation: clone, normalize stable project notes IDs, reconcile, validate,
compute actions, append and flush the complete frame, then commit SQLite plus its
marker. Only then expose the new state. A crash between flush and commit leaves a
pending operation that replays on startup. A SQLite failure after logging blocks
further writes until restart; the operation may therefore succeed during recovery
even if its original caller received an error. Record IDs make replay idempotent.

An incomplete frame at a file's tail is ignored as an interrupted append. New
sessions write a separate file and never append after that tail. Corrupt complete
frames, invalid inventory, or unknown protocol versions fail startup explicitly.
Chunks are immutable after their owning session ends; do not reorder, edit, prune,
or mix independently generated histories. Version 1 handles one ordered history;
multi-writer convergence is handled by the mesh event projection. The version-2
`mesh` metadata is authoritative for merging; the human-readable action list remains
available for history. See the [mesh protocol](websocket-sync.md) for additive
stock, stable project IDs, conflict handling, migration, and per-origin cursors.

If no complete records or committed event index exist, the app emits one `initial: true` operation containing
add actions for all existing parts, purchases, projects, storage, allocations, and
manual BOM entries. KiCad-generated requests must be reconstructed by opening the
project file on that device. An empty database gets an empty initial record. Copy the entire action
directory to a fresh database's sibling `.actions` directory before opening it to
reconstruct inventory. Existing backups include both the action directory and
attachments. Do not replace a nonempty unrelated database with another history.

## Inspecting history

The **HEAD** popup lists operations with local dates, item names, and descriptive changes. **Refresh** reloads the newest page. `GET /v1/action-log` returns all decoded records
with chunk names and offsets through the existing optional loopback HTTP server.
It is read-only. `Store::action_log()` exposes the same data to Rust callers.

Protocol-5 sync stages concurrent field conflicts for explicit Common/Ours/Theirs
review. Resolution events retain both branch hashes as parents; notes resolutions
and drafts are described in [the sync workflow](websocket-sync.md).
