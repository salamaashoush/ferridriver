use crate::params::*;
use crate::server::{sess, ChromeyMcp};
use base64::Engine;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use rmcp::{handler::server::wrapper::Parameters, model::*, tool, tool_router, ErrorData};

#[tool_router(router = content_router, vis = "pub")]
impl ChromeyMcp {
    #[tool(name = "snapshot", description = "Take an accessibility snapshot of the page.")]
    async fn snapshot(&self, Parameters(p): Parameters<SnapshotParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        let snap = self.snap(&page, s).await;
        Ok(CallToolResult::success(vec![Content::text(snap)]))
    }

    #[tool(name = "screenshot", description = "Take a screenshot.")]
    async fn screenshot(&self, Parameters(p): Parameters<ScreenshotParams_>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        let format = match p.format.as_deref() {
            Some("jpeg") | Some("jpg") => CaptureScreenshotFormat::Jpeg,
            Some("webp") => CaptureScreenshotFormat::Webp,
            _ => CaptureScreenshotFormat::Png,
        };
        let mime = match format {
            CaptureScreenshotFormat::Jpeg => "image/jpeg",
            CaptureScreenshotFormat::Webp => "image/webp",
            _ => "image/png",
        };
        let bytes = if let Some(sel) = &p.selector {
            let el = page.find_element(sel).await.map_err(|e| Self::err(format!("{e}")))?;
            el.screenshot(format).await.map_err(|e| Self::err(format!("{e}")))?
        } else {
            let mut b = ScreenshotParams::builder().format(format);
            if let Some(q) = p.quality { b = b.quality(q); }
            if p.full_page.unwrap_or(false) { b = b.full_page(true); }
            page.screenshot(b.build()).await.map_err(|e| Self::err(format!("{e}")))?
        };
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(CallToolResult::success(vec![Content::image(b64, mime)]))
    }

    #[tool(name = "evaluate", description = "Evaluate JavaScript on the page.")]
    async fn evaluate(&self, Parameters(p): Parameters<EvaluateParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        let result = page.evaluate(p.expression.as_str()).await.map_err(|e| Self::err(format!("{e}")))?;
        let val = result.value()
            .map(|v| serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()))
            .unwrap_or_else(|| "undefined".to_string());
        Ok(CallToolResult::success(vec![Content::text(val)]))
    }

    #[tool(name = "get_content", description = "Get page HTML.")]
    async fn get_content(&self, Parameters(p): Parameters<SessionOnlyParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        let html = page.content().await.map_err(|e| Self::err(format!("{e}")))?;
        Ok(CallToolResult::success(vec![Content::text(html)]))
    }

    #[tool(name = "set_content", description = "Set page HTML.")]
    async fn set_content(&self, Parameters(p): Parameters<SetContentParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        let frame_id = page.mainframe().await
            .map_err(|e| Self::err(format!("No frame: {e}")))?
            .ok_or_else(|| Self::err("No main frame"))?;
        page.execute(chromiumoxide::cdp::browser_protocol::page::SetDocumentContentParams::new(frame_id, p.html.clone()))
            .await.map_err(|e| Self::err(format!("set_content: {e}")))?;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        self.action_ok(&page, s, "Content set.").await
    }

    #[tool(name = "wait_for", description = "Wait for selector or text to appear.")]
    async fn wait_for(&self, Parameters(p): Parameters<WaitForParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        let timeout_ms = p.timeout.unwrap_or(30000);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(Self::err("Timeout waiting for condition"));
            }
            if let Some(sel) = &p.selector {
                if page.find_element(sel).await.is_ok() {
                    return self.action_ok(&page, s, &format!("Found '{sel}'.")).await;
                }
            }
            if let Some(text) = &p.text {
                let js = format!("document.body?.innerText?.includes('{}') || false", text.replace('\'', "\\'"));
                if let Ok(r) = page.evaluate(js).await {
                    if r.value() == Some(&serde_json::Value::Bool(true)) {
                        return self.action_ok(&page, s, &format!("Found text '{text}'.")).await;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}
