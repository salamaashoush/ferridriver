use crate::params::*;
use crate::server::{sess, ChromeyMcp};
use rmcp::{handler::server::wrapper::Parameters, model::*, tool, tool_router, ErrorData};

#[tool_router(router = input_router, vis = "pub")]
impl ChromeyMcp {
    #[tool(name = "click", description = "Click an element by ref or selector.")]
    async fn click(&self, Parameters(p): Parameters<ClickParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        let ref_map = self.state.lock().await.ref_map(s);
        let el = Self::resolve(&page, &ref_map, &p.r#ref, &p.selector).await.map_err(|e| Self::err(e))?;
        if p.double_click.unwrap_or(false) {
            el.click().await.map_err(|e| Self::err(format!("{e}")))?;
            el.click().await.map_err(|e| Self::err(format!("{e}")))?;
        } else {
            el.click().await.map_err(|e| Self::err(format!("{e}")))?;
        }
        let target = p.r#ref.as_deref().or(p.selector.as_deref()).unwrap_or("?");
        self.action_ok(&page, s, &format!("Clicked '{target}'.")).await
    }

    #[tool(name = "click_at", description = "Click at X,Y coordinates.")]
    async fn click_at(&self, Parameters(p): Parameters<ClickAtParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        page.click(chromiumoxide::layout::Point { x: p.x, y: p.y }).await.map_err(|e| Self::err(format!("{e}")))?;
        self.action_ok(&page, s, &format!("Clicked at ({}, {}).", p.x, p.y)).await
    }

    #[tool(name = "hover", description = "Hover over an element.")]
    async fn hover(&self, Parameters(p): Parameters<HoverParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        let ref_map = self.state.lock().await.ref_map(s);
        let el = Self::resolve(&page, &ref_map, &p.r#ref, &p.selector).await.map_err(|e| Self::err(e))?;
        el.hover().await.map_err(|e| Self::err(format!("{e}")))?;
        let target = p.r#ref.as_deref().or(p.selector.as_deref()).unwrap_or("?");
        self.action_ok(&page, s, &format!("Hovered '{target}'.")).await
    }

    #[tool(name = "fill", description = "Fill an input element.")]
    async fn fill(&self, Parameters(p): Parameters<FillParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        let ref_map = self.state.lock().await.ref_map(s);
        let el = Self::resolve(&page, &ref_map, &p.r#ref, &p.selector).await.map_err(|e| Self::err(e))?;
        el.click().await.map_err(|e| Self::err(format!("{e}")))?;
        let _ = el.call_js_fn("function() { this.value = ''; }", false).await;
        el.type_str(&p.value).await.map_err(|e| Self::err(format!("{e}")))?;
        let target = p.r#ref.as_deref().or(p.selector.as_deref()).unwrap_or("?");
        self.action_ok(&page, s, &format!("Filled '{target}' with '{}'.", p.value)).await
    }

    #[tool(name = "fill_form", description = "Fill multiple form fields at once.")]
    async fn fill_form(&self, Parameters(p): Parameters<FillFormParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        let ref_map = self.state.lock().await.ref_map(s);
        let mut filled = Vec::new();
        for field in &p.fields {
            let el = Self::resolve(&page, &ref_map, &field.r#ref, &field.selector).await.map_err(|e| Self::err(e))?;
            el.click().await.map_err(|e| Self::err(format!("{e}")))?;
            let _ = el.call_js_fn("function() { this.value = ''; }", false).await;
            el.type_str(&field.value).await.map_err(|e| Self::err(format!("{e}")))?;
            let target = field.r#ref.as_deref().or(field.selector.as_deref()).unwrap_or("?");
            filled.push(format!("  {target} = '{}'", field.value));
        }
        self.action_ok(&page, s, &format!("Filled {} fields:\n{}", filled.len(), filled.join("\n"))).await
    }

    #[tool(name = "type_text", description = "Type text via keyboard.")]
    async fn type_text(&self, Parameters(p): Parameters<TypeTextParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        page.type_str(&p.text).await.map_err(|e| Self::err(format!("{e}")))?;
        self.action_ok(&page, s, &format!("Typed {} chars.", p.text.len())).await
    }

    #[tool(name = "press_key", description = "Press a key or combo.")]
    async fn press_key(&self, Parameters(p): Parameters<PressKeyParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        page.press_key(&p.key).await.map_err(|e| Self::err(format!("{e}")))?;
        self.action_ok(&page, s, &format!("Pressed '{}'.", p.key)).await
    }

    #[tool(name = "drag", description = "Drag between two points.")]
    async fn drag(&self, Parameters(p): Parameters<DragParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        let from = chromiumoxide::layout::Point { x: p.from_x, y: p.from_y };
        let to = chromiumoxide::layout::Point { x: p.to_x, y: p.to_y };
        page.click_and_drag(from, to).await.map_err(|e| Self::err(format!("{e}")))?;
        self.action_ok(&page, s, "Drag complete.").await
    }

    #[tool(name = "scroll", description = "Scroll the page.")]
    async fn scroll(&self, Parameters(p): Parameters<ScrollParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        if let Some(sel) = &p.selector {
            let el = page.find_element(sel).await.map_err(|e| Self::err(format!("{e}")))?;
            el.scroll_into_view().await.map_err(|e| Self::err(format!("{e}")))?;
        } else {
            page.evaluate(format!("window.scrollBy({}, {})", p.delta_x.unwrap_or(0.0), p.delta_y.unwrap_or(0.0)))
                .await.map_err(|e| Self::err(format!("{e}")))?;
        }
        self.action_ok(&page, s, "Scrolled.").await
    }
}
