#!/usr/bin/env python3

from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import argparse


class CrossOriginIsolatedHandler(SimpleHTTPRequestHandler):
    def end_headers(self) -> None:
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


parser = argparse.ArgumentParser()
parser.add_argument("--directory", type=Path, default=Path(__file__).resolve().parent.parent / "dist")
parser.add_argument("--port", type=int, default=4173)
args = parser.parse_args()
dist = args.directory.resolve()
handler = partial(CrossOriginIsolatedHandler, directory=dist)
ThreadingHTTPServer(("127.0.0.1", args.port), handler).serve_forever()
