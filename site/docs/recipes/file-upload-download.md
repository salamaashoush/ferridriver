# File upload and download

## Upload

`Locator::set_input_files` works on any `<input type="file">`. Pass an
absolute path or a list.

```rust
use ferridriver_test::prelude::*;

#[ferritest]
async fn uploads_avatar(ctx: TestContext) {
    let page = ctx.page().await?;
    page.goto("https://app.example.com/profile", None).await?;

    page.locator("input[type=file]")
        .set_input_files(vec!["fixtures/avatar.png".into()])
        .await?;

    page.locator("button[type=submit]").click().await?;
    expect(&page.locator(".upload-status"))
        .to_have_text("Uploaded")
        .await?;
}
```

Multiple files:

```rust
page.locator("input[type=file][multiple]")
    .set_input_files(vec![
        "fixtures/photo-1.jpg".into(),
        "fixtures/photo-2.jpg".into(),
    ])
    .await?;
```

## In-memory upload (no file on disk)

```rust
use ferridriver::options::FileChooserPayload;

page.locator("input[type=file]").set_input_files_from_payloads(
    vec![FileChooserPayload {
        name: "report.csv".into(),
        mime_type: "text/csv".into(),
        buffer: b"name,score\nAda,42\n".to_vec(),
    }],
).await?;
```

## Capture a download

```rust
use std::time::Duration;

let download = page
    .wait_for_download(30_000)
    .await
    .or_else(|_| async {
        // Trigger the download
        page.locator("a.download-csv").click().await?;
        page.wait_for_download(30_000).await
    })
    .await?;

let path = download.path().await?;
let bytes = std::fs::read(&path)?;
assert!(bytes.starts_with(b"name,score"));
```

Or save to a known path:

```rust
download.save_as("test-results/report.csv".into()).await?;
```

## File chooser events

For non-`<input>` triggers (a custom button that opens a native chooser
via JS):

```rust
let chooser = page.wait_for_file_chooser(10_000).await?;
chooser.set_files(vec!["fixtures/avatar.png".into()]).await?;
```

## TypeScript

```ts
// Upload
await page.locator('input[type=file]')
  .setInputFiles('./fixtures/avatar.png');

// In-memory
await page.locator('input[type=file]').setInputFiles({
  name: 'report.csv',
  mimeType: 'text/csv',
  buffer: Buffer.from('name,score\nAda,42\n'),
});

// Download
const download = await page.waitForDownload();
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
