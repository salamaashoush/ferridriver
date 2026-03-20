use crate::params::*;
use crate::server::{sess, ChromeyMcp};
use chromiumoxide::cdp::browser_protocol::emulation::{SetDeviceMetricsOverrideParams, SetGeolocationOverrideParams};
use chromiumoxide::cdp::browser_protocol::network::SetUserAgentOverrideParams;
use rmcp::{handler::server::wrapper::Parameters, model::*, tool, tool_router, ErrorData};

#[tool_router(router = emulation_router, vis = "pub")]
impl ChromeyMcp {
    #[tool(name = "emulate_device", description = "Emulate device viewport and user agent.")]
    async fn emulate_device(&self, Parameters(p): Parameters<EmulateDeviceParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        if p.width.is_some() || p.height.is_some() {
            let params = SetDeviceMetricsOverrideParams::new(
                p.width.unwrap_or(1280), p.height.unwrap_or(720),
                p.device_scale_factor.unwrap_or(1.0), p.mobile.unwrap_or(false),
            );
            page.emulate_viewport(params).await.map_err(|e| Self::err(format!("{e}")))?;
        }
        if let Some(ua) = &p.user_agent {
            page.set_user_agent(SetUserAgentOverrideParams::new(ua.clone()))
                .await.map_err(|e| Self::err(format!("{e}")))?;
        }
        Ok(CallToolResult::success(vec![Content::text("Device emulation applied.")]))
    }

    #[tool(name = "set_geolocation", description = "Set geolocation override.")]
    async fn set_geolocation(&self, Parameters(p): Parameters<SetGeolocationParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        let params = SetGeolocationOverrideParams::builder()
            .latitude(p.latitude).longitude(p.longitude).accuracy(p.accuracy.unwrap_or(1.0)).build();
        page.emulate_geolocation(params).await.map_err(|e| Self::err(format!("{e}")))?;
        Ok(CallToolResult::success(vec![Content::text(format!("Geolocation set to ({}, {}).", p.latitude, p.longitude))]))
    }
}
