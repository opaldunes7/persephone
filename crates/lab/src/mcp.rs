//! The tools over MCP on standard input and output, the way Claude Code and other agents connect.

use std::sync::Arc;

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt as _,
    model::{
        CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
    transport::stdio,
};
use serde_json::Value;

use crate::{Lab, tools::tools};

#[derive(Clone)]
struct Server {
    lab: Arc<Lab>,
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::new("persephone-lab", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "Persephone's markets, backtests and tests, with no limits on how you research. Call get_guide first: it lists short topics on how backtests work, writing strategies without code, settings and units.".into(),
        );
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools: Vec<Tool> = tools()
            .iter()
            .map(|tool| {
                let schema = match tool.input_schema() {
                    Value::Object(schema) => schema,
                    _ => serde_json::Map::new(),
                };
                Tool::new(tool.name, tool.what, Arc::new(schema))
            })
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let lab = Arc::clone(&self.lab);
        let name = request.name.to_string();
        let args = Value::Object(request.arguments.unwrap_or_default());
        // Backtests block, and each needs a thread of its own anyway.
        let answer = tokio::task::spawn_blocking(move || lab.dispatch(&name, &args))
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(match answer {
            Ok(value) => CallToolResult::success(vec![Content::text(value.to_string())]),
            Err(error) => CallToolResult::error(vec![Content::text(error)]),
        })
    }
}

/// Serves until the agent hangs up.
///
/// # Errors
///
/// Returns a message if the connection fails.
pub fn serve(lab: Arc<Lab>) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let service = Server { lab }
            .serve(stdio())
            .await
            .map_err(|e| e.to_string())?;
        service.waiting().await.map_err(|e| e.to_string())?;
        Ok(())
    })
}
