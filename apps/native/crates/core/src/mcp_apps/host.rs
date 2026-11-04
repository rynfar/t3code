use super::{MAX_MESSAGE_BYTES, PROTOCOL_VERSION, Reference, clamp_height};
use serde_json::{Value, json};
use std::collections::HashMap;

const TEARDOWN_ID: &str = "t3-host-0";

#[derive(Debug, Clone, PartialEq)]
pub enum Operation {
    CallTool { name: String, arguments: Value },
    ReadResource { uri: String },
    OpenLink { url: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Post(Value),
    Request { token: u64, operation: Operation },
    Height(u32),
    StartTeardownTimer,
    Close,
}

pub struct Host {
    app: Reference,
    context: Value,
    version: String,
    initialized: bool,
    disposed: bool,
    teardown_pending: bool,
    sent_context: Option<Value>,
    tool_call: Option<(Value, Option<Value>)>,
    tool_sent: bool,
    next_token: u64,
    pending: HashMap<u64, (Value, bool)>,
}

pub fn normalize_result(result: &Value) -> Value {
    let mut value = json!({"content":result.get("content").filter(|v| v.is_array()).cloned().unwrap_or(json!([]))});
    for key in ["structuredContent", "_meta"] {
        if let Some(field) = result.get(key).filter(|v| v.is_object() || v.is_array()) {
            value[key] = field.clone();
        }
    }
    if result.get("isError") == Some(&Value::Bool(true)) {
        value["isError"] = json!(true);
    }
    value
}

fn post(id: &Value, result: Value) -> Action {
    Action::Post(json!({"jsonrpc":"2.0", "id":id, "result":result}))
}
fn fail(id: &Value, code: i32, message: &str) -> Action {
    Action::Post(json!({"jsonrpc":"2.0", "id":id, "error":{"code":code,"message":message}}))
}
fn notify(method: &str, params: Value) -> Action {
    Action::Post(json!({"jsonrpc":"2.0", "method":method, "params":params}))
}

impl Host {
    pub fn new(app: Reference, context: Value, version: &str) -> Self {
        Self {
            app,
            context,
            version: version.into(),
            initialized: false,
            disposed: false,
            teardown_pending: false,
            sent_context: None,
            tool_call: None,
            tool_sent: false,
            next_token: 0,
            pending: HashMap::new(),
        }
    }

    pub fn receive(&mut self, text: &str) -> Vec<Action> {
        if self.disposed {
            return vec![];
        }
        let Ok(data) = serde_json::from_str::<Value>(text) else { return vec![] };
        if data.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return vec![];
        }
        let id = data.get("id").filter(|id| id.is_string() || id.is_number());
        let method = data.get("method").and_then(Value::as_str);
        if text.len() > MAX_MESSAGE_BYTES {
            return id
                .filter(|_| method.is_some())
                .map(|id| vec![fail(id, -32600, "Message too large.")])
                .unwrap_or_default();
        }
        let Some(method) = method else {
            return if self.teardown_pending && id.and_then(Value::as_str) == Some(TEARDOWN_ID) && (data.get("result").is_some() || data.get("error").is_some())
            {
                self.finish_teardown()
            } else {
                vec![]
            };
        };
        let params = &data["params"];
        let Some(id) = id else {
            return match method {
                "ui/notifications/initialized" if !self.initialized => {
                    self.initialized = true;
                    let mut actions = self.send_tool();
                    actions.extend(self.update_context(self.context.clone()));
                    actions
                }
                "ui/notifications/size-changed" => params["height"].as_f64().map(|h| vec![Action::Height(clamp_height(h))]).unwrap_or_default(),
                "ui/notifications/request-teardown" => self.teardown(),
                _ => vec![],
            };
        };
        let operation = match method {
            "ui/initialize" => {
                self.sent_context = Some(self.context.clone());
                return vec![post(
                    id,
                    json!({"protocolVersion":PROTOCOL_VERSION, "hostInfo":{"name":"t3-code","version":self.version}, "hostCapabilities":{"openLinks":{},"serverTools":{},"serverResources":{},"logging":{},"sandbox":{"csp":self.app.csp}}, "hostContext":self.context}),
                )];
            }
            "ping" => return vec![post(id, json!({}))],
            "tools/call" => {
                let name = params["name"].as_str().filter(|s| !s.trim().is_empty());
                let args = params.get("arguments").filter(|v| !v.is_null()).cloned().unwrap_or(json!({}));
                let Some(name) = name.filter(|_| args.is_object() || args.is_array()) else {
                    return vec![fail(id, -32602, "tools/call needs a tool name and object arguments.")];
                };
                Operation::CallTool {
                    name: name.into(),
                    arguments: args,
                }
            }
            "resources/read" => {
                let Some(uri) = params["uri"].as_str().filter(|s| !s.trim().is_empty()) else {
                    return vec![fail(id, -32602, "resources/read needs a uri.")];
                };
                Operation::ReadResource { uri: uri.into() }
            }
            "ui/open-link" => {
                let Some(url) = params["url"].as_str().filter(|s| {
                    s.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("http://")) || s.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("https://"))
                }) else {
                    return vec![fail(id, -32602, "Only http(s) links can be opened.")];
                };
                Operation::OpenLink { url: url.into() }
            }
            "ui/request-display-mode" => return vec![post(id, json!({"mode":"inline"}))],
            _ => return vec![fail(id, -32601, &format!("Method not found: {method}"))],
        };
        if self.pending.len() >= 16 {
            return vec![fail(id, -32000, "Too many requests in flight.")];
        }
        let token = self.next_token;
        self.next_token += 1;
        self.pending.insert(token, (id.clone(), matches!(operation, Operation::CallTool { .. })));
        vec![Action::Request { token, operation }]
    }

    pub fn complete(&mut self, token: u64, result: Result<Value, String>) -> Vec<Action> {
        let Some((id, tool)) = self.pending.remove(&token) else { return vec![] };
        if self.disposed {
            return vec![];
        }
        vec![match result {
            Ok(value) => post(&id, if tool { normalize_result(&value) } else { value }),
            Err(error) => fail(&id, -32000, if error.trim().is_empty() { "Request failed." } else { &error }),
        }]
    }

    pub fn set_tool_call(&mut self, arguments: Value, result: Option<Value>) -> Vec<Action> {
        if self.disposed {
            return vec![];
        }
        if !self.tool_sent {
            if self.tool_call.as_ref().is_none_or(|(_, result)| result.is_none()) {
                self.tool_call = Some((arguments, result));
            }
            return self.send_tool();
        }
        if let Some((_, current)) = &mut self.tool_call
            && current.is_none()
            && let Some(result) = result
        {
            *current = Some(result.clone());
            return vec![notify("ui/notifications/tool-result", normalize_result(&result))];
        }
        vec![]
    }

    fn send_tool(&mut self) -> Vec<Action> {
        let Some((arguments, result)) = &self.tool_call else { return vec![] };
        if !self.initialized || self.tool_sent {
            return vec![];
        }
        self.tool_sent = true;
        let mut actions = vec![notify(
            "ui/notifications/tool-input",
            json!({"arguments": if arguments.is_object() || arguments.is_array() { arguments.clone() } else { json!({}) }}),
        )];
        if let Some(result) = result {
            actions.push(notify("ui/notifications/tool-result", normalize_result(result)));
        }
        actions
    }

    pub fn update_context(&mut self, context: Value) -> Vec<Action> {
        self.context = context;
        if self.disposed || !self.initialized {
            return vec![];
        }
        let Some(previous) = self.sent_context.replace(self.context.clone()) else {
            return vec![];
        };
        let changes: serde_json::Map<_, _> = self
            .context
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(key, value)| previous.get(*key) != Some(*value))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if changes.is_empty() {
            vec![]
        } else {
            vec![notify("ui/notifications/host-context-changed", Value::Object(changes))]
        }
    }

    pub fn dispose(&mut self) {
        self.disposed = true;
        self.teardown_pending = false;
        self.pending.clear();
    }

    pub fn teardown(&mut self) -> Vec<Action> {
        if self.disposed || self.teardown_pending {
            return vec![];
        }
        if !self.initialized {
            return self.finish_teardown();
        }
        self.teardown_pending = true;
        vec![
            Action::Post(json!({"jsonrpc":"2.0","id":TEARDOWN_ID,"method":"ui/resource-teardown","params":{}})),
            Action::StartTeardownTimer,
        ]
    }

    pub fn teardown_timeout(&mut self) -> Vec<Action> {
        if self.teardown_pending { self.finish_teardown() } else { vec![] }
    }

    fn finish_teardown(&mut self) -> Vec<Action> {
        self.dispose();
        vec![Action::Close]
    }
}
