use crate::params::EmulateParams;
use crate::server::{sess, McpServer};
use ferridriver::options::ViewportConfig;
use rmcp::{handler::server::wrapper::Parameters, model::{CallToolResult, Content}, tool, tool_router, ErrorData};

#[tool_router(router = emulation_router, vis = "pub")]
impl McpServer {
    #[tool(name = "emulate", description = "Configure device emulation. Set viewport (width/height/scale/mobile), user agent, geolocation (lat/lng), and network conditions (offline/throttle) -- all optional, set any combination.")]
    async fn emulate(&self, Parameters(p): Parameters<EmulateParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(p.session.as_ref());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        let mut applied = Vec::new();

        // Viewport
        if p.width.is_some() || p.height.is_some() || p.device_scale_factor.is_some() || p.mobile.is_some() {
            let config = ViewportConfig {
                width: p.width.unwrap_or(1280),
                height: p.height.unwrap_or(720),
                device_scale_factor: p.device_scale_factor.unwrap_or(1.0),
                is_mobile: p.mobile.unwrap_or(false),
                ..Default::default()
            };
            page.set_viewport(&config).await.map_err(Self::err)?;
            applied.push(format!("viewport {}x{}", config.width, config.height));
        }

        // User agent
        if let Some(ua) = &p.user_agent {
            page.set_user_agent(ua).await.map_err(Self::err)?;
            applied.push("user agent".into());
        }

        // Geolocation
        if let Some(lat) = p.latitude {
            let lng = p.longitude.unwrap_or(0.0);
            let acc = p.accuracy.unwrap_or(1.0);
            page.set_geolocation(lat, lng, acc).await.map_err(Self::err)?;
            applied.push(format!("geolocation ({lat}, {lng})"));
        }

        // Network
        if let Some(ref net) = p.network {
            let offline = net == "offline";
            page.set_network_state(
                offline,
                p.latency.unwrap_or(0.0),
                p.download_throughput.unwrap_or(-1.0),
                p.upload_throughput.unwrap_or(-1.0),
            ).await.map_err(Self::err)?;
            applied.push(format!("network: {net}"));
        }

        if applied.is_empty() {
            return Err(Self::err("No emulation options provided. Set at least one of: width/height, user_agent, latitude, network."));
        }

        Ok(CallToolResult::success(vec![Content::text(format!("Emulation applied: {}.", applied.join(", ")))]))
    }
}
