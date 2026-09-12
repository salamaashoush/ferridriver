import argparse
import asyncio
import json
import math
import os
import pathlib
import platform
import statistics
import tempfile
import time
import urllib.parse


class Client:
    def __init__(self, process):
        self.process = process
        self.pending = {}
        self.next_id = 0
        self.wire_bytes = 0
        self.reader = asyncio.create_task(self.read())

    async def read(self):
        try:
            while line := await self.process.stdout.readline():
                self.wire_bytes += len(line)
                response = json.loads(line)
                future = self.pending.pop(response.get("id"), None)
                if future is not None:
                    future.set_result(response)
            if self.pending:
                raise RuntimeError("MCP closed with requests pending")
        except Exception as error:
            for future in self.pending.values():
                if not future.done():
                    future.set_exception(error)

    async def request(self, method, params):
        self.next_id += 1
        request_id = self.next_id
        future = asyncio.get_running_loop().create_future()
        self.pending[request_id] = future
        self.process.stdin.write((json.dumps({
            "jsonrpc": "2.0", "id": request_id, "method": method, "params": params,
        }) + "\n").encode())
        await self.process.stdin.drain()
        response = await asyncio.wait_for(future, 30)
        if "error" in response or response.get("result", {}).get("isError"):
            raise RuntimeError(json.dumps(response))
        return response

    async def call(self, name, arguments):
        return await self.request("tools/call", {"name": name, "arguments": arguments})

    async def close(self):
        self.process.stdin.close()
        try:
            await asyncio.wait_for(self.process.wait(), 5)
        except TimeoutError:
            self.process.terminate()
            await asyncio.wait_for(self.process.wait(), 5)
        await self.reader


def aggregate(samples, elapsed):
    ordered = sorted(samples)
    return {
        "samples": len(samples), "p50_ms": statistics.median(samples),
        "p95_ms": ordered[math.ceil(len(samples) * .95) - 1],
        "p99_ms": ordered[math.ceil(len(samples) * .99) - 1],
        "throughput_per_second": len(samples) / elapsed,
    }


async def measure(options, name, command, navigation, evaluation, close_tool):
    root = pathlib.Path(tempfile.mkdtemp(prefix=f"ferridriver-bench-{name}-"))
    (root / "agent-browser.json").write_text("{}\n")
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(("FERRIDRIVER_", "AGENT_BROWSER_"))}
    environment.update(FERRIDRIVER_NO_INHERIT="1", FERRIDRIVER_SESSION_DIR=str(root / "sessions"),
                       AGENT_BROWSER_SESSION=root.name, AGENT_BROWSER_EXECUTABLE_PATH=options.chromium,
                       AGENT_BROWSER_CONFIG=str(root / "agent-browser.json"))
    start = time.perf_counter()
    with (root / "stderr.log").open("wb") as stderr:
        process = await asyncio.create_subprocess_exec(
            *command, cwd=root, env=environment, stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE, stderr=stderr,
        )
        client = Client(process)
        try:
            initialized = await client.request("initialize", {
                "protocolVersion": "2024-11-05", "capabilities": {},
                "clientInfo": {"name": "ferridriver-comparison", "version": "1"},
            })
            process.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
            await process.stdin.drain()
            await client.call(*navigation)

            async def operation():
                start = time.perf_counter()
                response = await client.call(*evaluation)
                duration = (time.perf_counter() - start) * 1000
                assert "fd-bench-4950" in json.dumps(response), response
                return duration

            await operation()
            cold_ms = (time.perf_counter() - start) * 1000
            for _ in range(options.warmup):
                await operation()
            workloads = {}
            for concurrency in [1, 4]:
                samples = []
                bytes_before = client.wire_bytes
                started = time.perf_counter()
                for offset in range(0, options.samples, concurrency):
                    samples.extend(await asyncio.gather(*[
                        operation() for _ in range(min(concurrency, options.samples - offset))
                    ]))
                workloads[str(concurrency)] = aggregate(samples, time.perf_counter() - started)
                workloads[str(concurrency)]["response_bytes_per_request"] = (client.wire_bytes - bytes_before) / len(samples)
            status = pathlib.Path(f"/proc/{process.pid}/status")
            rss = next((line.split()[1] for line in status.read_text().splitlines() if line.startswith("VmRSS:")), None) if status.exists() else None
            return {
                "server": initialized["result"]["serverInfo"], "command": command,
                "cold_start_navigation_evaluation_ms": cold_ms, "warm": workloads,
                "mcp_frontend_rss_kib_excludes_browser_and_daemon": int(rss) if rss else None,
                "stderr": str(root / "stderr.log"),
            }
        finally:
            try:
                if close_tool:
                    await client.call(close_tool, {})
            finally:
                await client.close()


async def run(options):
    version = await asyncio.create_subprocess_exec(options.chromium, "--version", stdout=asyncio.subprocess.PIPE)
    chromium_version, _ = await version.communicate()
    if version.returncode:
        raise RuntimeError(f"Chromium version probe failed: {version.returncode}")
    html = "<title>Protocol benchmark</title><ul>" + "".join(f"<li>{i}</li>" for i in range(100)) + "</ul>"
    url = "data:text/html," + urllib.parse.quote(html)
    function = "() => 'fd-bench-' + Array.from(document.querySelectorAll('li')).reduce((sum, el) => sum + Number(el.textContent), 0)"
    tools = [
        ("ferridriver", [str(pathlib.Path(options.ferridriver).resolve()), "mcp", "--no-inherit", "--headless", "--backend", "cdp-pipe", "--executable-path", options.chromium],
         ("navigate", {"url": url}), ("run_script", {"source": f"return await page.evaluate({function});"}), None),
        ("agent-browser", [options.agent_browser, "mcp"],
         ("agent_browser_open", {"url": url}), ("agent_browser_eval", {"script": f"({function})()"}), "agent_browser_close"),
        ("chrome-devtools-mcp", [options.chrome_devtools_mcp, "--headless", "--isolated", "--executable-path", options.chromium,
                                "--no-page-id-routing", "--no-usage-statistics", "--no-performance-crux"],
         ("navigate_page", {"type": "url", "url": url}),
         ("evaluate_script", {"function": function, "waitForStableDom": False}), None),
    ]
    results = {"platform": platform.platform(), "chromium": options.chromium,
               "chromium_version": chromium_version.decode().strip(), "samples": options.samples,
               "warmup": options.warmup, "workload": "Read and sum the same 100 DOM list items through persistent MCP", "tools": {}}
    for name, command, navigation, evaluation, close_tool in tools:
        results["tools"][name] = await measure(options, name, command, navigation, evaluation, close_tool)
        print(name + ": " + json.dumps(results["tools"][name]), flush=True)
    if options.output:
        with open(options.output, "x") as output:
            json.dump(results, output, indent=2)
            output.write("\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--ferridriver", default="target/debug/ferridriver")
    parser.add_argument("--agent-browser", required=True)
    parser.add_argument("--chrome-devtools-mcp", required=True)
    parser.add_argument("--chromium", default="/usr/bin/chromium")
    parser.add_argument("--samples", type=int, default=100)
    parser.add_argument("--warmup", type=int, default=5)
    parser.add_argument("--output")
    asyncio.run(run(parser.parse_args()))
