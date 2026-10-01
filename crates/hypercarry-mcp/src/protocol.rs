use crate::{
    config::{Config, Scope},
    tools,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::atomic::AtomicBool};

#[derive(Default)]
pub struct Session {
    initialized: bool,
    ready: bool,
}
#[allow(clippy::needless_pass_by_value)] // Response takes ownership of its request ID.
fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
impl Session {
    #[allow(clippy::needless_pass_by_value)] // One owned frame per request.
    pub fn handle(
        &mut self,
        config: &Config,
        scopes: &BTreeSet<Scope>,
        request: Value,
        stop: &AtomicBool,
    ) -> Option<Value> {
        let id = request.get("id").cloned();
        if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || request.get("method").and_then(Value::as_str).is_none()
            || id
                .as_ref()
                .is_some_and(|id| !(id.is_string() || id.is_i64() || id.is_u64()))
        {
            return Some(error(Value::Null, -32600, "Invalid Request"));
        }
        let method = request["method"].as_str()?;
        // A notification must never execute a tool or create an approval.
        let Some(id) = id else {
            if method == "notifications/initialized" && self.initialized {
                self.ready = true;
            }
            return None;
        };
        let params = request.get("params").cloned().unwrap_or(json!({}));
        let result = match method {
            "initialize" => {
                if self.initialized
                    || !params["protocolVersion"].is_string()
                    || !params["capabilities"].is_object()
                    || !params["clientInfo"].is_object()
                {
                    return Some(error(id, -32602, "Invalid initialization"));
                }
                self.initialized = true;
                json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"hypercarry","version":env!("CARGO_PKG_VERSION")},"instructions":tools::CONFIDENCE})
            }
            "ping" => json!({}),
            _ if !self.ready => {
                return Some(error(
                    id,
                    -32002,
                    "Initialize the session before using tools",
                ));
            }
            "tools/list" => {
                if params.get("cursor").is_some() {
                    return Some(error(id, -32602, "This tool list is not paginated"));
                }
                json!({"tools":tools::list(scopes)})
            }
            "tools/call" => {
                let Some(name) = params["name"].as_str() else {
                    return Some(error(id, -32602, "Tool name required"));
                };
                if !tools::list(scopes).iter().any(|tool| tool["name"] == name) {
                    return Some(error(id, -32602, "Tool is unavailable in this scope"));
                }
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                if !args.is_object() {
                    return Some(error(id, -32602, "Tool arguments must be an object"));
                }
                let value =
                    tools::envelope(config, name, tools::call(config, scopes, name, args, stop));
                json!({"isError":!value["error"].is_null(),"content":[{"type":"text","text":value.to_string()}],"structuredContent":value})
            }
            _ => return Some(error(id, -32601, "Method not found")),
        };
        Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
    }
}
