import argparse
import asyncio
import http.server
import hashlib
import json
import pathlib
import tempfile
import threading
import time


HTML = """<!doctype html><meta name="viewport" content="width=device-width">
<style>body{font:20px sans-serif;padding:20px;background:#102030;color:white}
input,button,select{font:20px sans-serif;display:block;margin:16px 0;padding:8px}
#result{color:#76e3ac}iframe{width:95%;height:100px}</style>
<h1>ferridriver on Android</h1><label>Name<input id="name"></label>
<select id="choice"><option value="one">One</option><option value="two">Two</option></select>
<button id="save" onclick="document.querySelector('#result').textContent='Verified: '+document.querySelector('#name').value+' / '+document.querySelector('#choice').value;window.lastTrusted=event.isTrusted">Save</button>
<p id="result">Running script...</p><iframe id="frame" srcdoc="<input id='inside' aria-label='Inside'>"></iframe>
"""


NAPI_RUNNER = """
const { chromium, firefox, webkit } = await import(settings.module);
const factory = settings.backend === 'bidi' ? firefox() : settings.backend === 'webkit' ? webkit() :
  chromium({transport: settings.backend === 'cdp-raw' ? 'ws' : 'pipe'});
const browser = settings.backend ? await factory.launch({headless:true}) :
  await factory.connect(settings.endpoint, {timeout:120000, capabilities:settings.capabilities});
let value, duration_ms;
try {
  const context = settings.backend ? await browser.newContext() : browser.contexts()[0];
  const page = settings.backend ? await context.newPage() : (await context.pages())[0];
  const start = performance.now();
  const AsyncFunction = (async function() {}).constructor;
  value = await new AsyncFunction('page', 'context', 'browser', 'args', settings.source)(page, context, browser, settings.args);
  duration_ms = Math.round(performance.now() - start);
} finally { await browser.close(); }
console.log(JSON.stringify({status:'ok',value,duration_ms}));
"""


class Fixture(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = HTML.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


async def run(options):
    root = pathlib.Path(tempfile.mkdtemp(prefix="ferridriver-android-appium-"))
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    capabilities = {
        "platformName": "Android", "browserName": "Chrome",
        "appium:options": {"automationName": "UiAutomator2", "udid": options.device,
                           "deviceName": options.device, "newCommandTimeout": 120},
        "goog:chromeOptions": {"args": ["--disable-fre", "--no-default-browser-check"]},
    }
    config = root / "ferridriver.json"
    target = ({"backend": options.backend, "headless": True} if options.backend else {
        "connectUrl": options.endpoint,
        "connectOptions": {"timeout": 120000, "capabilities": capabilities},
    })
    config.write_text(json.dumps({"browser": {"instances": {"target": target}},
        "test": {"workers": 1, "browser": {"instance": "target"}}}))
    source = (pathlib.Path(__file__).parent / "fixtures/browser-workflow.js").read_text()
    host = "127.0.0.1" if options.backend else "10.0.2.2"
    url = f"http://{host}:{server.server_port}/"
    (root / "workflow.js").write_text(source)
    try:
        command = [str(pathlib.Path(options.binary).resolve()), "run", "--no-inherit", "--config", str(config),
                   "--instance", "target", "--fresh", "--json", "--trace", "-e", source,
                   "--", url, str(root / "browser.png")]
        if options.runtime == "napi":
            module = pathlib.Path(__file__).resolve().parents[1] / "crates/ferridriver-node/index.js"
            bootstrap = root / "napi-run.mjs"
            settings = {"module": module.as_uri(), "endpoint": options.endpoint, "capabilities": capabilities,
                        "backend": options.backend, "source": source, "args": [url, str(root / "browser.png")]}
            bootstrap.write_text("const settings = " + json.dumps(settings) + ";\n" + NAPI_RUNNER)
            command = ["bun", str(bootstrap)]
        if options.runtime == "test":
            test_file = root / "workflow.test.ts"
            arguments = json.dumps([url, str(root / "browser.png")])
            test_file.write_text("import {test} from '@ferridriver/test';\n"
                "import {writeFileSync} from 'node:fs';\n"
                "async function workflow(page, context, browser, args) {\n" + source + "\n}\n"
                "test('configured target runs the shared browser workflow', async ({page, context, browser}) => {\n"
                "const value = await workflow(page, context, browser, " + arguments + ");\n"
                "writeFileSync('workflow-result.json', JSON.stringify({status:'ok', value}));\n});\n")
            command = [str(pathlib.Path(options.binary).resolve()), "test", "--no-inherit", "--config", str(config),
                       str(test_file), "--workers", "1"]
        started = time.perf_counter()
        process = await asyncio.create_subprocess_exec(
            *command, cwd=root, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
        )
        stdout, stderr = await process.communicate()
        metadata = {
            "runtime": options.runtime, "target": options.backend or "android-appium",
            "workflowSha256": hashlib.sha256(source.encode()).hexdigest(),
            "processWallMs": round((time.perf_counter() - started) * 1000, 3),
            "workflowExitCode": process.returncode,
        }
        (root / "result.json").write_bytes(stdout)
        (root / "actions.log").write_bytes(stderr)
        print(stderr.decode(), end="")
        print(stdout.decode(), end="")
        print(f"Artifacts: {root}")
        try:
            if process.returncode:
                raise RuntimeError(f"ferridriver exited {process.returncode}")
            result = json.loads((root / "workflow-result.json").read_text()) if options.runtime == "test" else json.loads(stdout)
            assert result["status"] == "ok", result
            if not options.backend:
                device = result["value"]["device"]
                assert "Android" in device["userAgent"] and device["width"] < 600 and device["touch"] >= 1, device
            assert (root / "browser.png").stat().st_size > 1000
            metadata["validationStatus"] = "passed"
        except Exception as error:
            metadata["validationStatus"] = "failed"
            metadata["validationError"] = str(error)
            raise
        finally:
            (root / "probe.json").write_text(json.dumps(metadata, indent=2) + "\n")
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/ferridriver")
    parser.add_argument("--runtime", choices=["script", "napi", "test"], default="script")
    parser.add_argument("--endpoint", default="http://127.0.0.1:4725")
    parser.add_argument("--device", default="emulator-5580")
    parser.add_argument("--backend", choices=["cdp-pipe", "cdp-raw", "bidi", "webkit"],
                        help="Run the identical script against a local desktop backend instead of Appium")
    asyncio.run(run(parser.parse_args()))
