# File upload and download

## Upload

`Locator::set_input_files` works on any `<input type="file">`. Pass an
absolute path or a list.

```rust
use ferridriver_test::prelude::*;
use ferridriver::options::InputFiles;

#[ferritest]
async fn uploads_avatar(ctx: TestContext) {
    let page = ctx.page().await?;
    page.goto("https://app.example.com/profile", None).await?;

    page.locator("input[type=file]", None)
        .set_input_files(InputFiles::Paths(vec!["fixtures/avatar.png".into()]), None)
        .await?;

    page.locator("button[type=submit]", None).click(None).await?;
    expect(&page.locator(".upload-status", None))
        .to_have_text("Uploaded")
        .await?;
}
```

Multiple files:

```rust
page.locator("input[type=file][multiple]", None)
    .set_input_files(InputFiles::Paths(vec![
        "fixtures/photo-1.jpg".into(),
        "fixtures/photo-2.jpg".into(),
    ]), None)
    .await?;
```

## In-memory upload (no file on disk)

```rust
use ferridriver::options::{InputFiles, FilePayload};

page.locator("input[type=file]", None).set_input_files(
    InputFiles::Payloads(vec![FilePayload {
        name: "report.csv".into(),
        mime_type: "text/csv".into(),
        buffer: b"name,score\nAda,42\n".to_vec(),
    }]),
    None,
).await?;
```

## Capture a download

Trigger the download, then await the `download` event:

```rust
page.locator("a.download-csv", None).click(None).await?;
let download = page.wait_for_download(30_000).await?;

let path = download.path().await?;
let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
assert!(bytes.starts_with(b"name,score"));
```

Or save to a known path:

```rust
download.save_as(std::path::Path::new("test-results/report.csv")).await?;
```

## File chooser events

For non-`<input>` triggers (a custom button that opens a native chooser
via JS):

```rust
let chooser = page.wait_for_file_chooser(10_000).await?;
chooser.set_files(InputFiles::Paths(vec!["fixtures/avatar.png".into()]), None).await?;
```

## TypeScript

```ts
import type { Download } from '@ferridriver/node';

// Upload
await page.locator('input[type=file]')
  .setInputFiles('./fixtures/avatar.png');

// In-memory
await page.locator('input[type=file]').setInputFiles({
  name: 'report.csv',
  mimeType: 'text/csv',
  buffer: Buffer.from('name,score\nAda,42\n'),
});

// Download — waitForEvent returns a union; narrow it to Download.
const download = (await page.waitForEvent('download')) as Download;
await download.saveAs('./out/report.csv');
```

## Backend notes

| Backend | File upload | File download |
|---------|-------------|---------------|
| `cdp-pipe` / `cdp-raw` | yes | yes |
| `webkit`  | yes | yes |
| `bidi`    | yes | returns `Unsupported` on `waitForDownload` |

For BiDi, listen for the `framenavigated` event to a download URL and
fetch with `request` instead.
