# Git bundle sync

Project remote settings and commit announcements travel in the inventory
action log. The desktop Sync listener checks local Git branch tips every
five seconds while hosting. This catches commits made outside the app.
Each publisher has a separate tip in the project record so concurrent tips
can survive action replay. Local checkout paths and Git credentials stay
on their original device.

## Transfers and relay

A live Sync connection grants access to a separate `/git-bundle` WebSocket
on the same listener. It streams files in 64 KiB binary frames with a
4 GiB transfer limit. The response declares the optional base hash, tip
hash, byte length, and SHA-256 of the transferred bytes. The desktop
receiver writes to a temporary file and publishes it to the cache after
the length, checksum, and bundle header match. The browser verifies the
transfer checksum and streams Git pack validation through the shared disk-backed Grit adapter.

An announcement can arrive before the announcing commit is available
from the connected neighbour. Desktop receivers poll neighbours every
eight seconds and retry unavailable ranges. For A → B → C, B can relay A's
announcement immediately and serve the bundle after its own download
finishes. A desktop without a checkout can forward cached bytes. The
desktop receiver offers the current managed checkout HEAD as its base; subsequent
transfers can contain only new objects. A receiver without a usable base asks for
a full bundle. The endpoint can generate incremental bundles when
a requested base is an ancestor of the target. A cached exact range is
reused, with a cached full bundle as a fallback.

When a desktop project has no local checkout and one unambiguous announced tip,
the desktop creates one stable managed repository at
`inventory.git-bundles/<project-id>/repository/`. It contains `.git`, history
reachable from the shared tip, an index, and all tracked files. Initial creation
is staged and published atomically. Later bundles import their objects and record
`refs/vinventory/bundles/<tip>` without replacing the repository directory.

The checkout fast-forwards only when HEAD is an ancestor of the received tip and
the index/worktree are clean (including untracked files). Edits, staged changes,
detached HEAD, or divergent commits pause checkout updates while retaining received
history. Once resolved, periodic checks retry automatically. Clean linked checkouts also accept incoming fast-forwards; dirty or divergent
checkouts retain incoming history in refs without replacing working files. Managed repository updates are serialized per
project and use Grit's index locking and checkout safeguards; no Git process runs.

The previously attached per-commit repository is moved into the stable location,
preserving local edits and history. Other historical copies are retained rather
than deleted. Older KiCad-only snapshots migrate from retained bundles. The local
KiCad binding keeps its stable file path and advances its revision on successful
updates, refreshing parsed views. Cached bundles are checked on startup and
periodically, so downloads can be attached without retransmission.

The desktop imports into a ref under `refs/vinventory/bundles/<commit>`
when it has a linked repository. It leaves the working tree and branch
unchanged. A failed import can still be cached for relay. The
**Preserve Git bundles** preference controls whether locally generated
bundles are kept after serving them. Received relay bundles are kept so
a disconnected downstream peer can still ask for them later. Cache
files are not pruned automatically.

If two publishers announce competing tips, neither checkout is updated
by the other's bundle. Once a published checkout contains a competing tip
in its ancestry, the new announcement records that ancestry and future
fetches can focus on the resolved tip. The previous bundle files remain
available on disk.

## Browser

The WebAssembly target uses the local MIT-licensed fork under
`crates/grit-lib-wasm`. The [compatibility patch](../patches/grit-wasm.patch)
records the changes against upstream revision
`974e2801c06bfe2b6ae0982467f9666b94a878b9`. The browser uses
`bundle::read_header` and `objects::unpack_objects::ingest_pack_into` in a
dedicated worker, with Grit networking disabled. It does not use SSH,
HTTPS Git checkout, or native repository paths.

The browser polls announced Git tips in the background, downloading from
connected peers and retrying unavailable bundles. A single unresolved tip
automatically attaches its KiCad snapshot when the project has no local
link, then updates that managed link on subsequent commits. Cached snapshots
also apply offline. The storage worker checks the current announcements again
before committing the local binding; stale downloads cannot overwrite newer
or divergent tips. Updated files refresh loaded viewers without closing tabs.
Browser authoring folders accept clean incoming fast-forwards. Browser projects
can initialize and commit local repositories, sharing bundles without direct
Git network access. Competing tips pause automatic application until resolved.

**Preferences → Network → Version control → Preserve Git bundles**
keeps original bundle bytes with range metadata in browser-private
storage. A subsequent fetch can offer its last retained commit as the
base. The worker reads selected and retained bundles through disk-backed streaming readers. Competing tips also retain their raw bundles even
when the preference is off. Desktop nodes provide the relay servers.

Build with `./script/web.sh --build-only` after adding the stable
`wasm32-unknown-unknown` target. Run
`python3 script/test_git_bundle_browser.py` to exercise the packaged
WebAssembly with full, incremental, and truncated bundle fixtures in
Deno. Bundle encryption is reserved for a future protocol revision.

## Browser-hosted local repositories

Choose a KiCad directory with read/write access, save the project, then use its
Git icon (or **Edit project → Local version control…**). Initialize the repository,
review the changed files, enter an author and message, and commit. Grit runs in a
worker and writes standard loose objects, the index, and branch references into
that directory's `.git`. Locally installed Git can open the same repository and
push it; browser Git does not use SSH/HTTPS transport or execute hooks.

Attaching an existing browser checkout prepares and announces its current committed
HEAD when the project is saved. This does not stage or commit working changes. A
folder without a commit must first be initialized and committed using Local version
control.

Commits announce their tip through the existing history engine. A durable browser
outbox sends bundles over a separate `/git-bundle` WebSocket using the live sync
session's grant. The first upload is full; after acknowledgement, the next commit
uses an incremental bundle if its parent is the last acknowledged tip. A lost
base causes retry with the full bundle. Desktop receivers validate the announced
tip, length, checksum and Git pack and retain the range. Managed repositories
import only missing objects. Full relay bundles are generated on demand when a
peer has no matching base, rather than recompressing all history on receipt. Transfers and cache preparation run outside the UI thread. Outbox delivery
retries after interruptions and does not require a repository origin URL.

Outbox payload names include their SHA-256, so re-publishing the same commit cannot
overwrite bytes referenced by an earlier upload job. Legacy tip-named payloads
are rehashed before sending to recover stale checksum metadata. Failed uploads
appear in browser Git diagnostics and retry with a bounded backoff.

**Import received history** imports cached incoming bundles into
`refs/vinventory/bundles/<commit>` in the linked directory, preserving the current
branch and working files. Merge competing histories using local Git. Automatically
managed browser snapshots continue to update independently of physical folders.

The streaming adapter supports SHA-1 repositories with regular files. There is no
128 MiB repository or 32 MiB blob limit: payloads are read on demand in 64 KiB chunks.
A single transfer is limited to 4 GiB. Metadata budgets are 100,000 file paths,
250,000 objects per graph/pack, and 32 MiB for an index or other metadata object.
OPFS must have enough free space for temporary staged objects and bundles.
The worker returns journal ranges, and the main thread streams those ranges to
the granted directory, snapshot cache and durable outbox. Transfers use 1 MiB
acknowledgement windows to bound queued payloads. Checksums are incremental.
Loose and indexed packed objects, including OFS/REF deltas, are read lazily. Symlinks, submodules, sparse checkouts,
content filters, detached HEAD, pending merges/rebases, and reftable repositories
require local Git. Browser operations serialize through a browser lock, check for
Git lock files, and recheck branch/index contents before publishing. Avoid running
another Git writer in the same directory while a browser operation is active.

Bundle availability is also signalled over the service connection: receiving
peers wake immediately after adopting new history or a bundle-ready message.
The eight-second polling interval remains a fallback for lost notifications.

## Desktop processing cost

Managed repositories supply their existing loose/packed object metadata to the
streaming importer. Known objects are not recompressed, and native checkout
preparation reads KiCad paths without copying unchanged PCB/schematic payloads
into the worker journal. Receipt validates/imports once; applying the new tip
reuses those objects. Transfer and checkout remain background tasks.

The development desktop profile optimizes Grit, SHA-1/SHA-256 and zlib-rs. This
also applies to native push packing, without changing repository configuration
or disabling deltas. Initial transfers and pushes still process the required
history; incremental operations avoid repeated full-history work.

## Receiver checkout folder changes

Changing the folder of a received project validates the destination before
changing its local binding. An empty folder receives the repository. A non-empty
folder must contain a Git working repository whose HEAD exactly matches the
currently attached commit; otherwise selection fails and the existing binding
stays in place. Matching repositories retain their working files, including any
local edits, and continue safe automatic updates. Local edits or divergence still
pause checkout updates. Desktop relocation runs as a background job.

Browser physical receiver folders use the same Grit/WASM checkout planner as
bundle processing. Only changed tracked files are written, removed tracked files
are deleted, and the branch advances after objects and index are written. The
planner checks the last applied HEAD and rejects overwriting local changes or
untracked content. Browser authoring folders that already published a tip also
accept safe incoming fast-forwards from desktop peers. A desktop can publish new
commits made in a managed checkout; previously received tips are not echoed as
new local commits. Local folder bindings never change another device's path.

Browser folder selection verifies that both HEAD and the Git index exist before
accepting the new binding. An interrupted initial copy can resume if its existing
files match a received commit or an ancestor already copied into its object store;
changed or unrelated files are never overwritten. An older recovered checkout then
fast-forwards normally. KiCad's temporary `~*.kicad_pro.lck`, `~*.kicad_sch.lck`
and `~*.kicad_pcb.lck` files can be rejected by Chromium's native filesystem API.
Their Git objects and history remain intact, but browser checkouts omit these
process-local files and mark their index entries skip-worktree. Browser commits
preserve those entries rather than staging deletions.

Browser background reconciliation waits for sync adoption and persistence to
finish, including the Rust store projection, before updating requested rows.
Legacy bindings without an applied-tip suffix recover using the actual folder
HEAD and the same clean-tree/ancestry checks. Polling reads only Git refs for an
already applied physical folder; a projection marker alone is not proof that
checkout completed. Project edit keeps the validated folder chooser available.
A “git tips updated” history entry announces commit hashes and known ancestry;
it does not acknowledge bundle receipt or checkout. Blocked application reports
“Git update pending” with a reason instead of silently appearing successful.

## Project Git history and diagnostics

The history icon beside each project's notes opens a read-only dialog on desktop
and browser. The Git log tab starts with 30 commits and loads another 30 when
scrolled to the bottom, without a fixed history limit. The Diagnostics tab shows
each publisher's advertised hash, the viewer binding's applied hash, actual
checkout HEAD, and received bundle ranges. The tree icon turns red for blocked
application or competing histories and returns to yellow after resolution.
Browser diagnostics include bounded local receive/apply/block activity for the
current session; native diagnostics retain the latest automatic checkout error
in memory. Announcements are not remote checkout acknowledgements. “Copy
diagnostics” captures this distinction for troubleshooting without resetting data.
History loading uses background Grit object reads, including cached bundles when
the browser attachment is only a snapshot. It does not stage or commit files.

Browser commit controls remain visible for attached directories after they are
fast-forwarded by sync. Internal extracted snapshots explicitly report read-only
status rather than offering to initialize a new repository in the snapshot.
A failed cached update for one browser project no longer aborts polling the rest.


## Bidirectional transfer and concurrent work

Browsers initiate both downloads and uploads on short-lived `/git-bundle`
WebSockets. The persistent `/sync` socket carries tip announcements and readiness
notifications. Desktop never needs to connect back to an inbound browser port.
Readiness notifications refresh the session's bulk-transfer grant; the browser
also remembers its joined gateway and retries after reload or temporary loss.
An explicit Disconnect cancels automatic reconnection.

Each received tip is imported into `refs/vinventory/bundles/<hash>` of a linked
repository before attempting checkout. Clean ancestral updates fast-forward.
Concurrent tips and local edits do not overwrite the branch or files; diagnostics
show the conflict and the incoming refs remain available for manual resolution.
A merge commit that contains both histories is advertised with proven ancestry,
which lets all nodes converge. Browser folder polling also detects committed HEAD
changes made by external tools and asks Rust to announce them.

The streaming reader expands compressed delta bases once into its temporary disk
journal. Delta copy instructions then seek directly within that journal. This
avoids repeated recursive decompression of large schematic histories while
retaining bounded memory and checksum validation.

## Automatic cache cleanup

Desktop and browser maintenance runs at most once per minute and leaves files
created in the last ten minutes alone. It never deletes Git objects, refs,
working files, or user-linked directories. The preservation preference disables
raw bundle/outbox pruning. Pending transfers, unresolved histories, and relays
without a complete applied repository retain the history they need.

Desktop caches discard recognized bundle ranges only when the project has one
resolved tip, the checkout and managed viewer have applied that tip, and the
range's commits are reachable in the repository. Active transfers/application
hold shared cache guards; cleanup takes an exclusive guard. A later request
regenerates the requested full or incremental bundle using Grit. Receivers check
the repository HEAD after pruning rather than downloading the same tip again.

Browser repositories keep a full baseline for the applied HEAD. If necessary,
the Grit worker recreates that baseline from repository objects before removing
older reachable ranges. This does not publish a commit or replace an upload job.
Current upload-job files remain protected, including while offline; superseded
outbox payloads can be removed. Read-only snapshots without repository objects
retain their raw history. Superseded internal snapshot directories and their
browser handle records are removed only when no current project or announced tip
references them. Conflict snapshots remain protected. Temporary transfer and
worker journal cleanup continues independently.
