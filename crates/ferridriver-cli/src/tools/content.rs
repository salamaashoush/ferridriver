use ferridriver::options::ScreenshotOptions;
use crate::params::*;
use crate::server::{sess, McpServer};
use base64::Engine;
use rmcp::{handler::server::wrapper::Parameters, model::*, tool, tool_router, ErrorData};

#[tool_router(router = content_router, vis = "pub")]
impl McpServer {
    #[tool(name = "snapshot", description = "Take an accessibility snapshot of the page.")]
    async fn snapshot(&self, Parameters(p): Parameters<SnapshotParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
        let page = self.page(s).await?;
        let snap = self.snap(&page, s).await;
        Ok(CallToolResult::success(vec![Content::text(snap)]))
    }

    #[tool(name = "screenshot", description = "Take a screenshot.")]
    async fn screenshot(&self, Parameters(p): Parameters<ScreenshotParams_>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
        let page = self.page(s).await?;
        let mime = match p.format.as_deref() {
            Some("jpeg") | Some("jpg") => "image/jpeg",
            Some("webp") => "image/webp",
            _ => "image/png",
        };
        let bytes = if let Some(sel) = &p.selector {
            page.screenshot_element(sel).await.map_err(|e| Self::err(e))?
        } else {
            let opts = ScreenshotOptions {
                format: p.format.clone(),
                quality: p.quality,
                full_page: p.full_page,
            };
            page.screenshot(opts).await.map_err(|e| Self::err(e))?
        };
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(CallToolResult::success(vec![Content::image(b64, mime)]))
    }

    #[tool(name = "evaluate", description = "Evaluate JavaScript on the page.")]
    async fn evaluate(&self, Parameters(p): Parameters<EvaluateParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
        let page = self.page(s).await?;
        let result = page.evaluate(p.expression.as_str()).await.map_err(|e| Self::err(e))?;
        let val = result
            .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()))
            .unwrap_or_else(|| "undefined".to_string());
        Ok(CallToolResult::success(vec![Content::text(val)]))
    }

    #[tool(name = "get_content", description = "Get page HTML.")]
    async fn get_content(&self, Parameters(p): Parameters<SessionOnlyParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
        let page = self.page(s).await?;
        let html = page.content().await.map_err(|e| Self::err(e))?;
        Ok(CallToolResult::success(vec![Content::text(html)]))
    }

    #[tool(name = "set_content", description = "Set page HTML.")]
    async fn set_content(&self, Parameters(p): Parameters<SetContentParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
        let page = self.page(s).await?;
        page.set_content(&p.html).await.map_err(|e| Self::err(e))?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if tokio::time::Instant::now() >= deadline { break; }
            match page.evaluate("document.readyState").await {
                Ok(Some(v)) if v.as_str() == Some("complete") || v.as_str() == Some("interactive") => break,
                _ => { tokio::time::sleep(std::time::Duration::from_millis(50)).await; }
            }
        }
        self.action_ok(&page, s, "Content set.").await
    }

    #[tool(name = "wait_for", description = "Wait for selector or text to appear.")]
    async fn wait_for(&self, Parameters(p): Parameters<WaitForParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
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
                if let Ok(r) = page.evaluate(&js).await {
                    if r == Some(serde_json::Value::Bool(true)) {
                        return self.action_ok(&page, s, &format!("Found text '{text}'.")).await;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(16)).await;
        }
    }

    #[tool(name = "search_page", description = "Search page text for a pattern (like grep). Zero cost, instant. Returns matches with surrounding context.")]
    async fn search_page(&self, Parameters(p): Parameters<SearchPageParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
        let page = self.page(s).await?;
        let opts = ferridriver::actions::SearchOptions {
            pattern: p.pattern.clone(),
            regex: p.regex.unwrap_or(false),
            case_sensitive: p.case_sensitive.unwrap_or(false),
            context_chars: p.context_chars.unwrap_or(150),
            css_scope: p.selector.clone(),
            max_results: p.max_results.unwrap_or(25),
        };
        let result = ferridriver::actions::search_page(page.inner(), &opts).await.map_err(|e| Self::err(e))?;
        Ok(CallToolResult::success(vec![Content::text(ferridriver::actions::format_search_results(&result, &p.pattern))]))
    }

    #[tool(name = "find_elements", description = "Query DOM elements by CSS selector. Zero cost, instant. Returns tag, text, and attributes.")]
    async fn find_elements(&self, Parameters(p): Parameters<FindElementsParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
        let page = self.page(s).await?;
        let opts = ferridriver::actions::FindElementsOptions {
            selector: p.selector.clone(),
            attributes: p.attributes.clone(),
            max_results: p.max_results.unwrap_or(50),
            include_text: p.include_text.unwrap_or(true),
        };
        let result = ferridriver::actions::find_elements(page.inner(), &opts).await.map_err(|e| Self::err(e))?;
        Ok(CallToolResult::success(vec![Content::text(ferridriver::actions::format_find_results(&result, &p.selector))]))
    }

    #[tool(name = "get_markdown", description = "Extract page content as clean markdown. More useful than raw HTML for reading and analysis.")]
    async fn get_markdown(&self, Parameters(p): Parameters<SessionOnlyParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
        let page = self.page(s).await?;
        let md = page.markdown().await.map_err(|e| Self::err(e))?;
        Ok(CallToolResult::success(vec![Content::text(md)]))
    }
}
