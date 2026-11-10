"""Exercise Grit bundle import through the browser build in Deno."""
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import tempfile
import zlib


def oid(kind, data):
    return hashlib.sha1(f"{kind} {len(data)}\0".encode() + data).hexdigest()


def pack_entry(kind, data):
    size = len(data)
    first = (kind << 4) | (size & 15)
    size >>= 4
    encoded = bytearray([first | (128 if size else 0)])
    while size:
        value = size & 127
        size >>= 7
        encoded.append(value | (128 if size else 0))
    return encoded + zlib.compress(data)


def write_bundle(path, content, base=None):
    blob = content.encode()
    tree = b"100644 board.kicad_pro\0" + bytes.fromhex(oid("blob", blob))
    parent = f"parent {base}\n" if base else ""
    commit = (
        f"tree {oid('tree', tree)}\n{parent}"
        "author Test <test@example.invalid> 1 +0000\n"
        "committer Test <test@example.invalid> 1 +0000\n\nTest commit\n"
    ).encode()
    tip = oid("commit", commit)
    prerequisite = f"-{base} base\n" if base else ""
    header = f"# v2 git bundle\n{prerequisite}{tip} refs/heads/main\n\n".encode()
    pack = b"PACK" + struct.pack(">II", 2, 3)
    pack += pack_entry(1, commit) + pack_entry(2, tree) + pack_entry(3, blob)
    path.write_bytes(header + pack + hashlib.sha1(pack).digest())
    return tip


def main():
    package = Path(__file__).resolve().parents[1] / "dist/web/pkg"
    module = package / "vispace.js"
    wasm = package / "vispace_bg.wasm"
    if not module.is_file() or not wasm.is_file():
        raise SystemExit("Build the browser app with ./script/web.sh --build-only")
    with tempfile.TemporaryDirectory() as directory:
        directory = Path(directory)
        full = directory / "full.bundle"
        incremental = directory / "incremental.bundle"
        base = write_bundle(full, "base")
        tip = write_bundle(incremental, "updated", base)
        script = directory / "test.mjs"
        script.write_text(
            f"""
import {{ initSync, git_bundle_header, ingest_git_bundle, kicad_files_at_git_commit }}
  from {json.dumps(module.as_uri())};
initSync({{module: Deno.readFileSync({json.dumps(str(wasm))})}});
const full = Deno.readFileSync({json.dumps(str(full))});
const incremental = Deno.readFileSync({json.dumps(str(incremental))});
function assert(value, message) {{ if (!value) throw Error(message); }}
function rejects(f, message) {{
  try {{ f(); }} catch (_) {{ return; }}
  throw Error(message);
}}
assert(JSON.parse(git_bundle_header(incremental)).bases[0] === '{base}', 'wrong base');
rejects(() => ingest_git_bundle(incremental), 'accepted missing base');
ingest_git_bundle(full);
rejects(() => ingest_git_bundle(incremental.slice(0, -8)), 'accepted truncated pack');
ingest_git_bundle(incremental);
const snapshot = JSON.parse(kicad_files_at_git_commit('{tip}'));
assert(snapshot.choices[0] === 'board.kicad_pro', 'wrong project');
assert(snapshot.files['board.kicad_pro'] === 'updated', 'wrong KiCad contents');
console.log('Browser WASM imported full and incremental Git bundles');
"""
        )
        subprocess.run(["deno", "run", "--allow-read", str(script)], check=True)


if __name__ == "__main__":
    main()
