import argparse
import asyncio
import json
import pathlib
import re
import tempfile
import urllib.parse
import urllib.request


async def run(options):
    driver = await asyncio.create_subprocess_exec(
        options.driver, "--port=0", stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.STDOUT
    )
    try:
        while True:
            line = (await asyncio.wait_for(driver.stdout.readline(), 10)).decode()
            match = re.search(r"started successfully on port (\d+)", line)
            if match:
                break
            if not line:
                raise RuntimeError("ChromeDriver exited before announcing its port")
        endpoint = f"http://127.0.0.1:{match[1]}"
        chrome = {"binary": options.chromium, "args": ["--headless=new", "--no-sandbox"]}
        capabilities = {"goog:chromeOptions": chrome}
        if options.classic:
            capabilities["webSocketUrl"] = False
        if options.mobile:
            chrome["mobileEmulation"] = {
                "deviceMetrics": {"width": 390, "height": 844, "pixelRatio": 3, "touch": True, "mobile": True},
                "userAgent": "Mozilla/5.0 (Linux; Android 13) AppleWebKit/537.36 Chrome/149.0.0.0 Mobile Safari/537.36",
            }
        html = """<meta name="viewport" content="width=device-width"><input id="name">
<button onclick="const result=document.querySelector('#result');result.textContent=document.querySelector('#name').value;result.dataset.trusted=String(event.isTrusted)">Save</button>
<p id="result"></p>"""
        url = "data:text/html," + urllib.parse.quote(html)
        source = """
const browser = await chromium().connect(ENDPOINT, {timeout:10000, capabilities:CAPABILITIES});
try {
  const page = USE_DEVICE_PAGE ? (await browser.contexts()[0].pages())[0] : await browser.newPage();
  if (!page) throw new Error('driver did not expose its device page');
  await page.goto(URL);
  await page.locator('#name').fill('sashoush');
  await page.locator('button').click();
  const value = await page.locator('#result').textContent();
  if (value !== 'sashoush') throw new Error(JSON.stringify(value));
  const trusted = await page.locator('#result').getAttribute('data-trusted');
  if (trusted !== 'true') throw new Error('click was not trusted');
  const metrics = await page.evaluate(() => ({width:innerWidth, ratio:devicePixelRatio, touch:navigator.maxTouchPoints}));
  return {version:browser.version(), value, trusted, metrics};
} finally {await browser.close();}
""".replace("ENDPOINT", json.dumps(endpoint)).replace("CAPABILITIES", json.dumps(capabilities)).replace("URL", json.dumps(url)).replace("USE_DEVICE_PAGE", json.dumps(options.mobile or options.classic))
        root = tempfile.mkdtemp(prefix="ferridriver-webdriver-probe-")
        if options.runtime == "napi":
            binding = pathlib.Path(__file__).resolve().parents[1] / "crates/ferridriver-node/index.js"
            script = pathlib.Path(root) / "probe.mjs"
            script.write_text(
                f"import {{chromium}} from {json.dumps(str(binding))};\n"
                f"const value = await (async () => {{{source}}})();\n"
                "console.log(JSON.stringify({status:'ok', value}));\n"
            )
            command = ["bun", str(script)]
        else:
            command = [str(pathlib.Path(options.binary).resolve()), "run", "--no-inherit", "--json", "-e", source]
        process = await asyncio.create_subprocess_exec(
            *command,
            cwd=root, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
        )
        try:
            stdout, stderr = await asyncio.wait_for(process.communicate(), 45)
        except BaseException:
            if process.returncode is None:
                process.kill()
                await process.wait()
            raise
        print(stdout.decode(), end="")
        if process.returncode:
            raise RuntimeError(f"ferridriver exited {process.returncode}: {stderr.decode()}")
        result = json.loads(stdout)
        assert result["status"] == "ok", result
        with urllib.request.urlopen(endpoint + "/sessions", timeout=5) as response:
            sessions = json.load(response)
        assert sessions["value"] == [], sessions
        if options.mobile:
            metrics = result["value"]["metrics"]
            if isinstance(metrics, str):
                metrics = json.loads(metrics)
            assert metrics == {"width": 390, "ratio": 3, "touch": 1}, metrics
    finally:
        if driver.returncode is None:
            driver.terminate()
            await asyncio.wait_for(driver.wait(), 10)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/ferridriver")
    parser.add_argument("--driver", default="/usr/bin/chromedriver")
    parser.add_argument("--chromium", default="/usr/bin/chromium")
    parser.add_argument("--mobile", action="store_true")
    parser.add_argument("--classic", action="store_true")
    parser.add_argument("--runtime", choices=["script", "napi"], default="script")
    asyncio.run(run(parser.parse_args()))
