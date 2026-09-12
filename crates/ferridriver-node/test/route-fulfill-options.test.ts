import { expect, test } from "bun:test";
import { resolve } from "node:path";
import { launchForBackend } from "./_helpers.js";

for (const backend of ["cdp-pipe", "cdp-raw", "bidi", "webkit"]) {
  test(`${backend}: fulfillment options reach the browser`, async () => {
    const server = Bun.serve({ port: 0, fetch: () => new Response("upstream") });
    const browser = await launchForBackend(backend);
    try {
      const page = await browser.newPage();
      await page.goto(server.url.href);
      const upstream = await page.request.get(server.url.href);
      await page.route("**/mock/*", async route => {
        const name = new URL(route.request().url()).pathname.split("/").pop();
        if (name === "json") await route.fulfill({ json: { name: "sashoush" }, status: 201, headers: { "X-Example": "yes", "Content-Type": "wrong" } });
        else if (name === "null") await route.fulfill({ json: null });
        else if (name === "bytes") await route.fulfill({ body: Buffer.from([0, 128, 255]), contentType: "application/octet-stream" });
        else if (name === "path") await route.fulfill({ path: resolve("../../tests/e2e/helpers/fixture.css") });
        else if (name === "response") await route.fulfill({ response: upstream });
        else if (name === "both") {
          await expect(route.fulfill({ json: {}, body: "conflict" })).rejects.toThrow("Can specify either body or json parameters");
          await route.fulfill({ status: 409, body: "recovered" });
        }
      });
      const fetchMock = (name: string) => page.evaluate(`fetch('/mock/${name}').then(async r => JSON.stringify({status:r.status,type:r.headers.get('content-type'),custom:r.headers.get('x-example'),body:await r.text()}))`).then(value => JSON.parse(String(value)));
      expect(await fetchMock("json")).toEqual({ status: 201, type: "application/json", custom: "yes", body: '{"name":"sashoush"}' });
      expect((await fetchMock("null")).body).toBe("null");
      expect(String(await page.evaluate("fetch('/mock/bytes').then(r=>r.arrayBuffer()).then(b=>Array.from(new Uint8Array(b)).join(','))"))).toBe("0,128,255");
      const file = await fetchMock("path");
      expect(file.type).toContain("text/css");
      expect(file.body).toContain("--fulfilled-from-disk");
      expect((await fetchMock("response")).body).toBe("upstream");
      expect(await fetchMock("both")).toMatchObject({ status: 409, body: "recovered" });
    } finally {
      await browser.close();
      server.stop(true);
    }
  });
}
