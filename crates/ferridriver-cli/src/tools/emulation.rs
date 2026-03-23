use crate::params::*;
use crate::server::{sess, McpServer};
use ferridriver::options::ViewportConfig;
use rmcp::{handler::server::wrapper::Parameters, model::*, tool, tool_router, ErrorData};

#[tool_router(router = emulation_router, vis = "pub")]
impl McpServer {
    #[tool(name = "emulate_device", description = "Emulate device viewport and user agent.")]
    async fn emulate_device(&self, Parameters(p): Parameters<EmulateDeviceParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
        let page = self.page(s).await?;
        if p.width.is_some() || p.height.is_some() {
            let config = ViewportConfig {
                width: p.width.unwrap_or(1280),
                height: p.height.unwrap_or(720),
                device_scale_factor: p.device_scale_factor.unwrap_or(1.0),
                is_mobile: p.mobile.unwrap_or(false),
                ..Default::default()
            };
            page.set_viewport(&config).await.map_err(|e| Self::err(e))?;
        }
        if let Some(ua) = &p.user_agent {
            page.set_user_agent(ua).await.map_err(|e| Self::err(e))?;
        }
        Ok(CallToolResult::success(vec![Content::text("Device emulation applied.")]))
    }

    #[tool(name = "set_geolocation", description = "Set geolocation override.")]
    async fn set_geolocation(&self, Parameters(p): Parameters<SetGeolocationParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
        let page = self.page(s).await?;
        page.set_geolocation(p.latitude, p.longitude, p.accuracy.unwrap_or(1.0)).await.map_err(|e| Self::err(e))?;
        Ok(CallToolResult::success(vec![Content::text(format!("Geolocation set to ({}, {}).", p.latitude, p.longitude))]))
    }
}
