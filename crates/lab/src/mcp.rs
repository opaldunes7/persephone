//! The tools over MCP on standard input and output, the way Claude Code and other agents connect.

use std::{sync::Arc, time::Duration};

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
            Ok(value) => {
                CallToolResult::success(vec![Content::text(self.lab.fit_for_agent(&value))])
            }
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

/// Passes the server's answers on to the agent.
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
        let (to_agent, from_server) = tokio::io::duplex(64 * 1024);
        let forwarding = tokio::spawn(forward(from_server));
        let service = Server { lab }
            .serve((tokio::io::stdin(), to_agent))
            .await
            .map_err(|e| e.to_string())?;
        service.waiting().await.map_err(|e| e.to_string())?;
        // Whatever's left to say, without hanging on it if the agent has stopped reading.
        let _ = tokio::time::timeout(Duration::from_secs(5), forwarding).await;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_error_with_no_id_gets_one() {
        let fixed: Value =
            serde_json::from_str(&with_id(r#"{"jsonrpc":"2.0","error":{"code":-32700}}"#)).unwrap();
        assert!(
            fixed["id"].is_null() && fixed.get("id").is_some(),
            "{fixed}"
        );
        for line in [
            r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32600}}"#,
            r#"{"jsonrpc":"2.0","id":4,"result":{"text":"{\"error\":1}"}}"#,
        ] {
            assert_eq!(with_id(line), line);
        }
    }
}
