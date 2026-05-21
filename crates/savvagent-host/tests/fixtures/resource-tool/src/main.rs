//! Minimal stdio MCP server used by the savvagent-host resource integration
//! test.
//!
//! Surface:
//!
//! - Tool `trigger_update` (no args). Each invocation publishes
//!   `notifications/resources/updated` for both `test://updated/payload-1`
//!   and `test://updated/payload-2` **before** returning a `"ok"` text result.
//! - `resources/list` returns an empty list (we never advertise the resources
//!   up-front; clients learn about them via the update notification).
//! - `resources/read` for both URIs returns a JSON `TextResourceContents` body
//!   of the shape `{"value": "<uri>"}` so callers can prove the read path
//!   reached this server.
//!
//! Notification ordering matters: the host's `ResourceCapturingHandler`
//! buffers updates and the test asserts that both arrived before the tool
//! result is observed by the model. We `.await` both `notify_resource_updated`
//! calls before returning from `call_tool`.

use std::{borrow::Cow, sync::Arc};

use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResult, Content, Implementation, JsonObject,
        ListResourcesResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
        ReadResourceRequestParams, ReadResourceResult, ResourceContents,
        ResourceUpdatedNotificationParam, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
    transport::stdio,
};
use serde_json::json;

const URI_ONE: &str = "test://updated/payload-1";
const URI_TWO: &str = "test://updated/payload-2";

#[derive(Clone, Default)]
struct Fixture;

impl ServerHandler for Fixture {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::default())
        .with_server_info(Implementation::new(
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
        ))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<rmcp::RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let schema: JsonObject = json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        })
        .as_object()
        .cloned()
        .expect("schema literal is an object");
        let tool = Tool::new(
            Cow::Borrowed("trigger_update"),
            Cow::Borrowed(
                "Publish two resources/updated notifications, then return \"ok\". Test fixture.",
            ),
            Arc::new(schema),
        );
        Ok(ListToolsResult::with_all_items(vec![tool]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if request.name != "trigger_update" {
            return Err(ErrorData::invalid_params(
                format!("unknown tool: {}", request.name),
                None,
            ));
        }

        // Order matters: notifications must be observable by the client before
        // the tool result returns. We `.await` each send so the server's
        // outbound queue has buffered both frames before we hand back a
        // success result.
        for uri in [URI_ONE, URI_TWO] {
            context
                .peer
                .notify_resource_updated(ResourceUpdatedNotificationParam {
                    uri: uri.to_string(),
                })
                .await
                .map_err(|e| {
                    ErrorData::internal_error(format!("notify_resource_updated failed: {e}"), None)
                })?;
        }

        Ok(CallToolResult::success(vec![Content::text("ok")]))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<rmcp::RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::default())
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<rmcp::RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let uri = request.uri;
        if uri != URI_ONE && uri != URI_TWO {
            return Err(ErrorData::invalid_params(
                format!("unknown resource uri: {uri}"),
                None,
            ));
        }
        let body = json!({ "value": uri }).to_string();
        Ok(ReadResourceResult::new(vec![
            ResourceContents::TextResourceContents {
                uri,
                mime_type: Some("application/json".to_string()),
                text: body,
                meta: None,
            },
        ]))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let service = Fixture.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
