# Vendored SQLite WASM

Official SQLite 3.54.1 module loader and WASM binary, extracted unchanged from:
https://sqlite.org/2026/sqlite-wasm-3540100.zip

Archive SHA3-256 (verified when vendoring):
87e24f6a4588692310b7942dc2fa7c7275bb9ba2e01d553e4115aa4a904a0050

The loader retains the upstream license notices. SQLite code is public domain;
Emscripten glue is distributed under its MIT/UIUC licensing described in that
header. The browser build copies sqlite3.mjs and sqlite3.wasm locally; no runtime
CDN dependency is required.

Storage uses installOpfsSAHPoolVfs in a dedicated worker, with one Web Lock owner.
