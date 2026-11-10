#!/usr/bin/env python3
"""Local-only static preview. No uploads, directory listings or inventory access."""
import argparse
import functools
import ssl
from pathlib import Path
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

class Handler(SimpleHTTPRequestHandler):
    extensions_map = {**SimpleHTTPRequestHandler.extensions_map, '.wasm': 'application/wasm', '.js': 'text/javascript'}
    def end_headers(self):
        self.send_header('Cache-Control', 'no-store')
        super().end_headers()
    def list_directory(self, path):
        self.send_error(404)
        return None

if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--directory', required=True)
    parser.add_argument('--port', type=int, default=8787)
    tls_dir = Path(__file__).resolve().parents[1] / ".local-tls"
    parser.add_argument("--cert", type=Path, default=tls_dir / "server.pem")
    parser.add_argument("--key", type=Path, default=tls_dir / "server-key.pem")
    parser.add_argument("--http", action="store_true", help="HTTP localhost only, for isolated tests")
    args = parser.parse_args()
    handler = functools.partial(Handler, directory=args.directory)
    server = ThreadingHTTPServer(('127.0.0.1', args.port), handler)
    if not args.http:
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.minimum_version = ssl.TLSVersion.TLSv1_2
        context.load_cert_chain(args.cert, args.key)
        server.socket = context.wrap_socket(server.socket, server_side=True)
    scheme = 'http' if args.http else 'https'
    print(f'Open {scheme}://localhost:{args.port} — press Ctrl+C to stop', flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
