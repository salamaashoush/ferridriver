use crate::params::{
  ClickAtParams, ClickParams, DragParams, FillFormParams, FillParams, HoverParams, PressKeyParams, ScrollParams,
  SelectOptionParams, TypeTextParams, UploadFileParams,
};
use crate::server::{McpServer, sess};
use rmcp::{ErrorData, handler::server::wrapper::Parameters, model::CallToolResult, tool, tool_router};

#[tool_router(router = input_router, vis = "pub")]
impl McpServer {
  #[tool(
    name = "click",
    description = "Click an element. Prefer 'ref' from snapshot (e.g. ref='e5') over CSS selector. Refs work across frames; CSS selectors only match the main frame."
  )]
  async fn click(&self, Parameters(p): Parameters<ClickParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;

    if let Some(r) = &p.r#ref {
      // Ref-based: resolve via snapshot refs, then click
      let ref_map = self.state.ref_map_for(s).await;
      let el = Self::resolve(&page, &ref_map, p.r#ref.as_ref(), p.selector.as_ref())
        .await
        .map_err(Self::err)?;
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

  #[tool(
    name = "click_at",
    description = "Click at exact X,Y viewport pixel coordinates. Use this only for canvas, maps, or elements without accessible refs. For interactive elements, prefer 'click' with a ref from snapshot."
  )]
  async fn click_at(&self, Parameters(p): Parameters<ClickAtParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    page.click_at(p.x, p.y).await.map_err(Self::err)?;
    self
      .action_ok(&page, s, &format!("Clicked at ({}, {}).", p.x, p.y))
      .await
  }

  #[tool(
    name = "hover",
    description = "Hover over an element. Prefer 'ref' from snapshot over CSS selector."
  )]
  async fn hover(&self, Parameters(p): Parameters<HoverParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    let target = p.r#ref.as_deref().or(p.selector.as_deref()).unwrap_or("?");
    if p.r#ref.is_some() {
      let ref_map = self.state.ref_map_for(s).await;
      let resolved = Self::resolve(&page, &ref_map, p.r#ref.as_ref(), p.selector.as_ref())
        .await
        .map_err(Self::err)?;
      resolved.hover().await.map_err(Self::err)?;
    } else if let Some(sel) = &p.selector {
      page.hover(sel).await.map_err(Self::err)?;
    } else {
      return Err(Self::err("Provide 'ref' or 'selector'."));
    }
    self.action_ok(&page, s, &format!("Hovered '{target}'.")).await
  }

  #[tool(
    name = "fill",
    description = "Fill an input or contenteditable element. Prefer 'ref' from snapshot over CSS selector. For contenteditable elements (e.g. WhatsApp message box), use type_text after clicking the element instead."
  )]
  async fn fill(&self, Parameters(p): Parameters<FillParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    let target = p.r#ref.as_deref().or(p.selector.as_deref()).unwrap_or("?");
    if p.r#ref.is_some() {
      let ref_map = self.state.ref_map_for(s).await;
      let _resolved = Self::resolve(&page, &ref_map, p.r#ref.as_ref(), p.selector.as_ref())
        .await
        .map_err(Self::err)?;
      page
        .locator("[data-fd-sel='0']")
        .fill(&p.value)
        .await
        .map_err(Self::err)?;
    } else if let Some(sel) = &p.selector {
      page.fill(sel, &p.value).await.map_err(Self::err)?;
    } else {
      return Err(Self::err("Provide 'ref' or 'selector'."));
    }
    self
      .action_ok(&page, s, &format!("Filled '{target}' with '{}'.", p.value))
      .await
  }

  #[tool(
    name = "fill_form",
    description = "Fill multiple form fields in a single call. More efficient than calling fill repeatedly. Each field is identified by ref (preferred) or CSS selector."
  )]
  async fn fill_form(&self, Parameters(p): Parameters<FillFormParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    let ref_map = self.state.ref_map_for(s).await;
    let mut filled = Vec::new();
    for field in &p.fields {
      let target = field.r#ref.as_deref().or(field.selector.as_deref()).unwrap_or("?");
      if let Some(sel) = &field.selector {
        page.fill(sel, &field.value).await.map_err(Self::err)?;
      } else if field.r#ref.is_some() {
        let _resolved = Self::resolve(&page, &ref_map, field.r#ref.as_ref(), field.selector.as_ref())
          .await
          .map_err(Self::err)?;
        let _ = page.locator("[data-fd-sel='0']").fill(&field.value).await;
      }
      filled.push(format!("  {target} = '{}'", field.value));
    }
    self
      .action_ok(
        &page,
        s,
        &format!("Filled {} fields:\n{}", filled.len(), filled.join("\n")),
      )
      .await
  }

  #[tool(
    name = "type_text",
    description = "Type text character-by-character via keyboard into the currently focused element. Click on the target element first using click(ref=...). Unlike fill, this fires keydown/keypress/keyup events per character -- use for contenteditable elements, rich text editors, or when keystroke events matter."
  )]
  async fn type_text(&self, Parameters(p): Parameters<TypeTextParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    page.keyboard().r#type(&p.text).await.map_err(Self::err)?;
    self
      .action_ok(&page, s, &format!("Typed {} chars.", p.text.len()))
      .await
  }

  #[tool(
    name = "press_key",
    description = "Press a keyboard key or shortcut combination. Supports named keys (Enter, Tab, Escape, ArrowDown) and combos (Control+a, Meta+v, Control+Shift+t). Uses Playwright key naming."
  )]
  async fn press_key(&self, Parameters(p): Parameters<PressKeyParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    page.keyboard().press(&p.key).await.map_err(Self::err)?;
    self.action_ok(&page, s, &format!("Pressed '{}'.", p.key)).await
  }

  #[tool(
    name = "drag",
    description = "Drag from one point to another using mouse down + move + up. Coordinates are in viewport pixels. Use for drag-and-drop interfaces, sliders, or resizable elements."
  )]
  async fn drag(&self, Parameters(p): Parameters<DragParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    let mouse = page.mouse();
    mouse.r#move(p.from_x, p.from_y, None).await.map_err(Self::err)?;
    mouse.down(None).await.map_err(Self::err)?;
    mouse.r#move(p.to_x, p.to_y, Some(10)).await.map_err(Self::err)?;
    mouse.up(None).await.map_err(Self::err)?;
    self.action_ok(&page, s, "Drag complete.").await
  }

  #[tool(
    name = "scroll",
    description = "Scroll the page by pixel delta or scroll a specific element into view. Use delta_y for vertical scroll (positive = down), or provide a CSS selector to auto-scroll that element into the viewport."
  )]
  async fn scroll(&self, Parameters(p): Parameters<ScrollParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    if let Some(sel) = &p.selector {
      page
        .locator(sel)
        .scroll_into_view_if_needed()
        .await
        .map_err(Self::err)?;
    } else {
      page
        .evaluate(&format!(
          "window.scrollBy({}, {})",
          p.delta_x.unwrap_or(0.0),
          p.delta_y.unwrap_or(0.0)
        ))
        .await
        .map_err(Self::err)?;
    }
    self.action_ok(&page, s, "Scrolled.").await
  }

  #[tool(
    name = "select_option",
    description = "Select an option in a <select> dropdown by value or label."
  )]
  async fn select_option(&self, Parameters(p): Parameters<SelectOptionParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    let target_text = p
      .label
      .as_deref()
      .or(p.value.as_deref())
      .ok_or_else(|| Self::err("Provide 'label' or 'value' to select."))?;
    let sel = p.selector.as_deref().or(p.r#ref.as_deref()).unwrap_or("select");
    let result = page.select_option(sel, target_text).await.map_err(Self::err)?;
    self
      .action_ok(
        &page,
        s,
        &format!("Selected '{}'.", result.first().unwrap_or(&String::new())),
      )
      .await
  }

  #[tool(
    name = "upload_file",
    description = "Upload a file to a file input. Prefer `ref` from snapshot; otherwise pass a CSS selector for the input."
  )]
  async fn upload_file(&self, Parameters(p): Parameters<UploadFileParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    let path_str = p.path.clone();
    let paths = std::slice::from_ref(&path_str);
    if p.r#ref.is_some() {
      let ref_map = self.state.ref_map_for(s).await;
      let _ = Self::resolve(&page, &ref_map, p.r#ref.as_ref(), None)
        .await
        .map_err(Self::err)?;
      page
        .set_input_files("[data-fd-sel='0']", paths)
        .await
        .map_err(Self::err)?;
      self
        .action_ok(
          &page,
          s,
          &format!(
            "Uploaded file '{}' to ref '{}'.",
            p.path,
            p.r#ref.as_deref().unwrap_or("")
          ),
        )
        .await
    } else if let Some(sel) = &p.selector {
      page.set_input_files(sel, paths).await.map_err(Self::err)?;
      self
        .action_ok(&page, s, &format!("Uploaded file '{}' to '{}'.", p.path, sel))
        .await
    } else {
      Err(Self::err(
        "Provide `ref` (from snapshot) or `selector` for the file input.",
      ))
    }
  }
}
