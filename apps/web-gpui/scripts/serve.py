#!/usr/bin/env python3

from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import argparse
from urllib.parse import urlsplit


class CrossOriginIsolatedHandler(SimpleHTTPRequestHandler):
    def send_head(self):
        source = Path(self.translate_path(self.path))
        if source.is_dir() and urlsplit(self.path).path.endswith("/"):
            for name in ("index.html", "index.htm"):
                index = source / name
                if index.is_file():
                    source = index
                    break
        if source.is_file() and source.suffix not in {".gz", ".br"}:
            accepted = {}
            for item in self.headers.get("Accept-Encoding", "").split(","):
                encoding, *parameters = item.strip().lower().split(";")
                quality = 1.0
                for parameter in parameters:
                    if parameter.strip().startswith("q="):
                        try:
                            quality = float(parameter.strip()[2:])
                        except ValueError:
                            quality = 0.0
                accepted[encoding] = quality if 0.0 <= quality <= 1.0 else 0.0
            encodings = sorted(("br", "gzip"), key=lambda encoding: accepted.get(encoding, accepted.get("*", 0.0)), reverse=True)
            for encoding in encodings:
                if accepted.get(encoding, accepted.get("*", 0.0)) <= 0:
                    continue
                sidecar = source.with_name(source.name + (".br" if encoding == "br" else ".gz"))
                try:
                    stream = sidecar.open("rb")
                except FileNotFoundError:
                    continue
                self.send_response(200)
                self.send_header("Content-Type", self.guess_type(str(source)))
                self.send_header("Content-Encoding", encoding)
                self.send_header("Content-Length", str(sidecar.stat().st_size))
                self.send_header("Last-Modified", self.date_time_string(source.stat().st_mtime))
                self.end_headers()
                return stream
        return super().send_head()

    def end_headers(self) -> None:
        google_login = urlsplit(self.path).path.endswith("/google_cloud_login.html")
        self.send_header("Cross-Origin-Opener-Policy", "same-origin-allow-popups" if google_login else "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "unsafe-none" if google_login else "require-corp")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Vary", "Accept-Encoding")
        super().end_headers()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--directory", type=Path, default=Path(__file__).resolve().parent.parent / "dist")
    parser.add_argument("--port", type=int, default=4173)
    args = parser.parse_args()
    dist = args.directory.resolve()
    handler = partial(CrossOriginIsolatedHandler, directory=dist)
    ThreadingHTTPServer(("127.0.0.1", args.port), handler).serve_forever()


if __name__ == "__main__":
    main()
