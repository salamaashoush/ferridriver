use crate::params::RunScenarioParams;
use crate::server::{sess, McpServer};
use rmcp::{handler::server::wrapper::Parameters, model::{CallToolResult, Content}, tool, tool_router, ErrorData};

#[tool_router(router = bdd_router, vis = "pub")]
impl McpServer {
    #[tool(name = "list_steps", description = "Show all step patterns supported by run_scenario. Call this before writing a scenario.")]
    async fn list_steps(&self) -> Result<CallToolResult, ErrorData> {
        Ok(CallToolResult::success(vec![Content::text(ferridriver::steps::StepRegistry::global().reference())]))
    }

    #[tool(name = "run_scenario", description = "Run a Gherkin scenario (Given/When/Then steps). All steps execute in one call. Call list_steps first to see available step patterns.")]
    async fn run_scenario(&self, Parameters(p): Parameters<RunScenarioParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(p.session.as_ref());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        let options = ferridriver::scenario::ScenarioOptions {
            stop_on_failure: p.stop_on_failure.unwrap_or(true),
            screenshot_on_failure: p.screenshot_on_failure.unwrap_or(false),
        };
        let result = ferridriver::scenario::run(page.inner(), &p.script, options).await.map_err(Self::err)?;
        let mut contents = vec![Content::text(serde_json::to_string_pretty(&result).unwrap_or_default())];
        for ss in &result.failure_screenshots {
            contents.push(Content::image(ss.base64.clone(), "image/png"));
        }
        if result.status == "failed" { Ok(CallToolResult::error(contents)) } else { Ok(CallToolResult::success(contents)) }
    }
}
