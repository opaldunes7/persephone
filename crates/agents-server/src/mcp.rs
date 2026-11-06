//! The controls over MCP on standard input and output, the way Claude Code and other AI clients
//! connect.

use std::{sync::Arc, time::Duration};

use persephone_agents::control::{Desk, tools};
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt as _,
    model::{
        CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncWriteExt as _, BufReader};

#[derive(Clone)]
struct Server {
    desk: Arc<Desk>,
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::new("persephone-agents", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "Persephone's agent teams: design teams of Claude agents (roles, models, who reports to whom, hand-offs and reviews), run them as sandboxed Claude Code sessions, and watch what they do. Teams show in the app's Agents view as they're changed. Call get_guide first.".into(),
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
        let desk = Arc::clone(&self.desk);
        let name = request.name.to_string();
        let args = Value::Object(request.arguments.unwrap_or_default());
        // Checking looks for Claude Code, which runs a program: off the async threads.
        let answer = tokio::task::spawn_blocking(move || desk.dispatch(&name, &args))
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(match answer {
            Ok(value) => CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&value).unwrap_or_default(),
            )]),
            Err(error) => CallToolResult::error(vec![Content::text(error)]),
        })
    }
}

/// rmcp leaves `id` out of its answer to a message it couldn't read, where JSON-RPC asks for a null
/// one, and strict clients reject that.
fn with_id(line: &str) -> String {
    if line.contains("\"error\":")
        && let Ok(Value::Object(mut answer)) = serde_json::from_str(line)
        && answer.contains_key("error")
        && !answer.contains_key("id")
    {
        answer.insert("id".into(), Value::Null);
        return Value::Object(answer).to_string();
    }
    line.to_owned()
}

/// Passes the server's answers on to the client.
async fn forward(from_server: impl AsyncRead + Unpin) {
    let mut lines = BufReader::new(from_server).lines();
    let mut stdout = tokio::io::stdout();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = with_id(&line) + "\n";
        if stdout.write_all(line.as_bytes()).await.is_err() || stdout.flush().await.is_err() {
            return;
        }
    }
}

/// Serves until the client hangs up.
///
/// # Errors
///
/// Returns a message if the connection fails.
pub fn serve(desk: Desk) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let (to_client, from_server) = tokio::io::duplex(64 * 1024);
        let forwarding = tokio::spawn(forward(from_server));
        let service = Server {
            desk: Arc::new(desk),
        }
        .serve((tokio::io::stdin(), to_client))
        .await
        .map_err(|e| e.to_string())?;
        service.waiting().await.map_err(|e| e.to_string())?;
        let _ = tokio::time::timeout(Duration::from_secs(5), forwarding).await;
        Ok(())
    })
}
