#!/usr/bin/env python3
import http.server
import os
import socketserver
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
os.chdir(ROOT)
PORT = 8765
Handler = http.server.SimpleHTTPRequestHandler
with socketserver.TCPServer(("127.0.0.1", PORT), Handler) as httpd:
    print(f"Tenzor Live Lab: http://127.0.0.1:{PORT}/live_lab/")
    httpd.serve_forever()
