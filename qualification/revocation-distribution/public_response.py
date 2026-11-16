"""Serve a public response fixture and record that a request reached it."""

import argparse
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--port', type=int, required=True)
    parser.add_argument('--response', type=Path, required=True)
    parser.add_argument('--marker', type=Path, required=True)
    arguments = parser.parse_args()

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            arguments.marker.write_text('received')
            self.send_response(200)
            self.end_headers()
            self.wfile.write(arguments.response.read_bytes())

        def log_message(self, *details):
            pass

    HTTPServer(('0.0.0.0', arguments.port), Handler).serve_forever()


if __name__ == '__main__':
    main()
