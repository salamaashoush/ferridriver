import http.server, socketserver, threading, time, struct, zlib

def png(w, h):
    def chunk(t, d):
        c = t + d
        return struct.pack(">I", len(d)) + c + struct.pack(">I", zlib.crc32(c) & 0xffffffff)
    raw = b"".join(b"\x00" + bytes([(x*7) % 256, (x*3) % 256, 90]*w) for x in range(h))
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 1))
            + chunk(b"IEND", b""))

HERO = png(600, 400)
BIG_CSS = ("body{font-family:sans-serif;margin:0}\n" + "/* pad */\n" * 4000).encode()
# A babel-transpiled bundle: a core-js polyfill plus a class transform.
LEGACY_JS = (b"function _classCallCheck(a,n){if(!(a instanceof n))throw new TypeError("
             b"\"Cannot call a class as a function\");}\n"
             b"Array.prototype.at = function(i){return this[i<0?this.length+i:i];};\n"
             b"String.prototype.trimEnd = function(){return this.replace(/\\s+$/,'');};\n"
             b"Object.fromEntries = function(e){var o={};for(var p of e)o[p[0]]=p[1];return o;};\n"
             + b"// pad\n" * 200)

class First(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def do_GET(self):
        hdrs = {}
        if self.path == "/":
            body = (b"<html><head><meta charset=utf-8>"
                    b"<link rel=stylesheet href='/slow.css'>"
                    b"</head><body>"
                    # LCP image: lazy, no fetchpriority, injected late by script
                    b"<script>document.write(\"<img src='/hero.png' loading='lazy' width=600 height=400>\")</script>"
                    b"<h1>perf fixture</h1>"
                    b"<script src='http://localhost:8733/tracker.js'></script>"
                    b"<script src='/legacy.js'></script>"
                    b"</body></html>")
            ctype = "text/html"
        elif self.path == "/slow.css":
            time.sleep(0.25); body = BIG_CSS; ctype = "text/css"
        elif self.path == "/legacy.js":
            body = LEGACY_JS; ctype = "text/javascript"
        elif self.path == "/hero.png":
            time.sleep(0.15); body = HERO; ctype = "image/png"
            hdrs["Cache-Control"] = "max-age=60"      # short: Cache insight should flag it
        else:
            self.send_response(404); self.send_header("Content-Length","0"); self.end_headers(); return
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        for k,v in hdrs.items(): self.send_header(k,v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers(); self.wfile.write(body)

class Third(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def do_GET(self):
        body = b"window.__t=1;\n" * 500
        self.send_response(200)
        self.send_header("Content-Type","text/javascript")
        self.send_header("Cache-Control","max-age=30")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers(); self.wfile.write(body)

class Threaded(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True; allow_reuse_address = True
threading.Thread(target=lambda: Threaded(("127.0.0.1",8732), First).serve_forever(), daemon=True).start()
threading.Thread(target=lambda: Threaded(("127.0.0.1",8733), Third).serve_forever(), daemon=True).start()
print("up", flush=True); time.sleep(3600)
