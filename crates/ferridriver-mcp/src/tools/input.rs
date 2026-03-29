use crate::params::{ClickParams, ClickAtParams, HoverParams, FillParams, FillFormParams, TypeTextParams, PressKeyParams, DragParams, ScrollParams, SelectOptionParams, UploadFileParams};
use crate::server::{sess, McpServer};
use rmcp::{handler::server::wrapper::Parameters, model::CallToolResult, tool, tool_router, ErrorData};

#[tool_router(router = input_router, vis = "pub")]
impl McpServer {
    #[tool(name = "click", description = "Click an element by ref or selector.")]
    async fn click(&self, Parameters(p): Parameters<ClickParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(p.session.as_ref());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;

        if let Some(r) = &p.r#ref {
            // Ref-based: resolve via snapshot refs, then click
            let ref_map = self.state.lock().await.ref_map(s);
            let el = Self::resolve(&page, &ref_map, p.r#ref.as_ref(), p.selector.as_ref()).await.map_err(Self::err)?;
            if p.double_click.unwrap_or(false) {
                el.click().await.map_err(Self::err)?;
                el.click().await.map_err(Self::err)?;
            } else {
                el.click().await.map_err(Self::err)?;
            }
            self.action_ok(&page, s, &format!("Clicked '{r}'.")).await
        } else if let Some(sel) = &p.selector {
            // Selector-based: use Page API
            if p.double_click.unwrap_or(false) {
                page.locator(sel).dblclick().await.map_err(Self::err)?;
            } else {
                page.click(sel).await.map_err(Self::err)?;
            }
            self.action_ok(&page, s, &format!("Clicked '{sel}'.")).await
        } else {
            Err(Self::err("Provide 'ref' or 'selector'."))
        }
    }

    #[tool(name = "click_at", description = "Click at X,Y coordinates.")]
    async fn click_at(&self, Parameters(p): Parameters<ClickAtParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(p.session.as_ref());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        page.click_at(p.x, p.y).await.map_err(Self::err)?;
        self.action_ok(&page, s, &format!("Clicked at ({}, {}).", p.x, p.y)).await
    }

    #[tool(name = "hover", description = "Hover over an element.")]
    async fn hover(&self, Parameters(p): Parameters<HoverParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(p.session.as_ref());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        let target = p.r#ref.as_deref().or(p.selector.as_deref()).unwrap_or("?");
        if p.r#ref.is_some() {
            let ref_map = self.state.lock().await.ref_map(s);
            let resolved = Self::resolve(&page, &ref_map, p.r#ref.as_ref(), p.selector.as_ref()).await.map_err(Self::err)?;
            resolved.hover().await.map_err(Self::err)?;
        } else if let Some(sel) = &p.selector {
            page.hover(sel).await.map_err(Self::err)?;
        } else {
            return Err(Self::err("Provide 'ref' or 'selector'."));
        }
        self.action_ok(&page, s, &format!("Hovered '{target}'.")).await
    }

    #[tool(name = "fill", description = "Fill an input element.")]
    async fn fill(&self, Parameters(p): Parameters<FillParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(p.session.as_ref());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        let target = p.r#ref.as_deref().or(p.selector.as_deref()).unwrap_or("?");
        if p.r#ref.is_some() {
            let ref_map = self.state.lock().await.ref_map(s);
            let _resolved = Self::resolve(&page, &ref_map, p.r#ref.as_ref(), p.selector.as_ref()).await.map_err(Self::err)?;
            page.locator("[data-fd-sel='0']").fill(&p.value).await.map_err(Self::err)?;
        } else if let Some(sel) = &p.selector {
            page.fill(sel, &p.value).await.map_err(Self::err)?;
        } else {
            return Err(Self::err("Provide 'ref' or 'selector'."));
        }
        self.action_ok(&page, s, &format!("Filled '{target}' with '{}'.", p.value)).await
    }

    #[tool(name = "fill_form", description = "Fill multiple form fields at once.")]
    async fn fill_form(&self, Parameters(p): Parameters<FillFormParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(p.session.as_ref());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        let mut filled = Vec::new();
        for field in &p.fields {
            let target = field.r#ref.as_deref().or(field.selector.as_deref()).unwrap_or("?");
            if let Some(sel) = &field.selector {
                page.fill(sel, &field.value).await.map_err(Self::err)?;
            } else if field.r#ref.is_some() {
                let ref_map = self.state.lock().await.ref_map(s);
                let _resolved = Self::resolve(&page, &ref_map, field.r#ref.as_ref(), field.selector.as_ref()).await.map_err(Self::err)?;
                let _ = page.locator("[data-fd-sel='0']").fill(&field.value).await;
            }
            filled.push(format!("  {target} = '{}'", field.value));
        }
        self.action_ok(&page, s, &format!("Filled {} fields:\n{}", filled.len(), filled.join("\n"))).await
    }

    #[tool(name = "type_text", description = "Type text via keyboard.")]
    async fn type_text(&self, Parameters(p): Parameters<TypeTextParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(p.session.as_ref());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        page.type_str(&p.text).await.map_err(Self::err)?;
        self.action_ok(&page, s, &format!("Typed {} chars.", p.text.len())).await
    }

    #[tool(name = "press_key", description = "Press a key or combo.")]
    async fn press_key(&self, Parameters(p): Parameters<PressKeyParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(p.session.as_ref());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        page.press_key(&p.key).await.map_err(Self::err)?;
        self.action_ok(&page, s, &format!("Pressed '{}'.", p.key)).await
    }

    #[tool(name = "drag", description = "Drag between two points.")]
    async fn drag(&self, Parameters(p): Parameters<DragParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(p.session.as_ref());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        page.drag_and_drop((p.from_x, p.from_y), (p.to_x, p.to_y)).await.map_err(Self::err)?;
        self.action_ok(&page, s, "Drag complete.").await
    }

    #[tool(name = "scroll", description = "Scroll the page.")]
    async fn scroll(&self, Parameters(p): Parameters<ScrollParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(p.session.as_ref());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        if let Some(sel) = &p.selector {
            page.locator(sel).scroll_into_view().await.map_err(Self::err)?;
        } else {
            page.evaluate(&format!("window.scrollBy({}, {})", p.delta_x.unwrap_or(0.0), p.delta_y.unwrap_or(0.0)))
                .await.map_err(Self::err)?;
        }
        self.action_ok(&page, s, "Scrolled.").await
    }

    #[tool(name = "select_option", description = "Select an option in a <select> dropdown by value or label.")]
    async fn select_option(&self, Parameters(p): Parameters<SelectOptionParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(p.session.as_ref());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        let target_text = p.label.as_deref().or(p.value.as_deref())
            .ok_or_else(|| Self::err("Provide 'label' or 'value' to select."))?;
        let sel = p.selector.as_deref().or(p.r#ref.as_deref()).unwrap_or("select");
        let result = page.select_option(sel, target_text).await.map_err(Self::err)?;
        self.action_ok(&page, s, &format!("Selected '{}'.", result.first().unwrap_or(&String::new()))).await
    }

    #[tool(name = "upload_file", description = "Upload a file to a file input element.")]
    async fn upload_file(&self, Parameters(p): Parameters<UploadFileParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(p.session.as_ref());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        page.set_input_files(&p.selector, std::slice::from_ref(&p.path)).await.map_err(Self::err)?;
        self.action_ok(&page, s, &format!("Uploaded file '{}' to '{}'.", p.path, p.selector)).await
    }
}
