"""The pages the differential fixtures were captured from.

Three of them, because one page cannot be both the simple case and the
loud one. `/` is a plain load with a render-blocking stylesheet, an
image LCP and a third-party script. `/rich` is everything the insights
look for that `/` leaves empty: a redirect, a text LCP, oversized
images, enough HTTP/1 requests on one origin to be worth reporting, a
layout shift, a large DOM and two byte-identical bundles. `/heavy` is
the two that need an opt-in trace category or a font request: a slow
web font with `font-display: block`, and a stylesheet whose selectors
cost real time to match against a deep DOM.

Serves on 8732, with a second origin on 8733.
"""

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
# Served at a tenth of its natural size, which is what ImageDelivery is
# looking for. Noisy pixels so it does not compress down to nothing.
OVERSIZED = png(1400, 1000)
BIG_CSS = ("body{font-family:sans-serif;margin:0}\n" + "/* pad */\n" * 4000).encode()
# A babel-transpiled bundle: a core-js polyfill plus a class transform.
LEGACY_JS = (b"function _classCallCheck(a,n){if(!(a instanceof n))throw new TypeError("
             b"\"Cannot call a class as a function\");}\n"
             b"Array.prototype.at = function(i){return this[i<0?this.length+i:i];};\n"
             b"String.prototype.trimEnd = function(){return this.replace(/\\s+$/,'');};\n"
             b"Object.fromEntries = function(e){var o={};for(var p of e)o[p[0]]=p[1];return o;};\n"
             + b"// pad\n" * 200)
# The same bytes at two URLs, which is the duplication a trace alone can
# show; duplicated modules inside two different bundles need source maps.
DUPLICATE_JS = (b"var __dup = (function(){\n"
                b"  function helper(a, b) { return a + b; }\n"
                b"  function other(a) { return helper(a, 1); }\n"
                b"  return { helper: helper, other: other };\n"
                b"})();\n" + b"// filler filler filler filler filler\n" * 400)
CHUNK_JS = b"window.__chunks = (window.__chunks || 0) + 1;\n"

# A large DOM, deeply nested, so DOMSize has something to count. Built
# once because building it per request shows up in the trace as server
# time.
def dom(count, depth):
    inner = "".join(f"<p class=row>row {i}</p>" for i in range(count))
    for level in range(depth):
        inner = f"<div class=lvl{level}>{inner}</div>"
    return inner

BIG_DOM = dom(1200, 14).encode()

# Selectors that cost real time to match: descendant chains the matcher
# has to walk up for every candidate, :not() over attributes, and
# :nth-child arithmetic. SelectorStats records the attempts and the time,
# which is what the SlowCSSSelector insight reads.
SLOW_SELECTORS = ("".join(
    f".lvl0 .lvl1 .lvl2 div:not([data-x='{i}']) .row:nth-child({i % 7 + 1}) {{ color: rgb({i % 255},0,0); }}\n"
    f"div > div > div > div > div .row:nth-of-type({i % 5 + 1}):not(.zz) {{ letter-spacing: {i % 3}px; }}\n"
    for i in range(400)
)).encode()

# Not a decodable font, and it does not need to be: Chrome emits
# BeginRemoteFontLoad when it starts fetching one, and the FontDisplay
# insight is arithmetic over that request's timings. A real font file
# would add a binary to the repository and change nothing the insight
# reads.
FONT_BODY = b"wOF2" + bytes(6000)

RICH = (b"<!doctype html><html><head>"
        b"<link rel=stylesheet href='/slow.css'>"
        b"<style>#lcp{font-size:64px;line-height:1.1;margin:0}"
        b".thumb{width:140px;height:100px}#shift{height:0}</style>"
        b"</head><body>"
        # The LCP is this heading, not an image, which is the branch that
        # makes a render-blocking saving count against LCP as well as FCP.
        b"<h1 id=lcp>A heading long enough to be the largest contentful paint on this page</h1>"
        b"<div id=shift></div>"
        b"<img class=thumb src='/wide-1.png'><img class=thumb src='/wide-2.png'>"
        b"<script src='/dup-a.js'></script>"
        b"<script src='/dup-b.js'></script>"
        b"<script src='/legacy.js'></script>"
        b"<script src='http://localhost:8733/tracker.js'></script>"
        b"<script src='/chunk-1.js'></script><script src='/chunk-2.js'></script>"
        b"<script src='/chunk-3.js'></script><script src='/chunk-4.js'></script>"
        b"<div id=dom>" + BIG_DOM + b"</div>"
        # Push everything down once the page has settled: a layout shift
        # with a culprit that is not an unsized image.
        b"<script>requestAnimationFrame(() => requestAnimationFrame(() => {"
        b"document.getElementById('shift').style.height = '220px'; }));</script>"
        b"</body></html>")


HEAVY = (b"<!doctype html><html><head><meta charset=utf-8>"
         b"<link rel=stylesheet href='/selectors.css'>"
         b"<style>@font-face{font-family:Fixture;src:url('/fixture.woff2') format('woff2');"
         b"font-display:block}"
         b"body{font-family:Fixture,serif}#lcp{font-size:56px;margin:0}</style>"
         b"</head><body>"
         b"<h1 id=lcp>Text set in a web font that has not arrived yet</h1>"
         b"<div id=dom>" + BIG_DOM + b"</div>"
         # Force a second style pass over the whole tree, so the
         # expensive selectors are matched more than once.
         b"<script>requestAnimationFrame(() => {"
         b"document.body.classList.add('recalc');"
         b"void document.body.offsetHeight; });</script>"
         b"</body></html>")


class First(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass

    def do_GET(self):
        hdrs = {}
        path = self.path
        # The rich page is reached through a redirect, so the document
        # request carries a redirect hop for DocumentLatency to report.
        if path == "/rich":
            self.send_response(302)
            self.send_header("Location", "/rich/")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if path == "/":
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
        elif path == "/heavy":
            body = HEAVY
            ctype = "text/html"
        elif path == "/selectors.css":
            body = SLOW_SELECTORS; ctype = "text/css"
        elif path == "/fixture.woff2":
            # Slow enough that font-display: block blocks visibly.
            time.sleep(0.3); body = FONT_BODY; ctype = "font/woff2"
        elif path == "/rich/":
            body = RICH
            # Declared in the header and not in the document, which is the
            # other half of the CharacterSet checklist.
            ctype = "text/html; charset=utf-8"
        elif path == "/slow.css":
            time.sleep(0.25); body = BIG_CSS; ctype = "text/css"
        elif path == "/legacy.js":
            body = LEGACY_JS; ctype = "text/javascript"
        elif path in ("/dup-a.js", "/dup-b.js"):
            body = DUPLICATE_JS; ctype = "text/javascript"
        elif path.startswith("/chunk-"):
            body = CHUNK_JS; ctype = "text/javascript"
        elif path.startswith("/wide-"):
            time.sleep(0.05); body = OVERSIZED; ctype = "image/png"
            hdrs["Cache-Control"] = "max-age=60"
        elif path == "/hero.png":
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
