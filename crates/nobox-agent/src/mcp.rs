//! MCP over stdio, in two dialects.
//!
//! The preferred revision is 2026-07-28, which is stateless: there is no
//! initialization handshake, a stdio process is explicitly not a session, and
//! every request carries its protocol version and client capabilities in
//! `_meta`. Cross-request state is passed back explicitly by the client —
//! which is what the seat's sequence numbers, client identities, and
//! generation counters already are.
//!
//! Speaking only that revision made this server unusable. Every host in the
//! field opens with `initialize`; this server answered `-32602`, the host
//! concluded the server was broken, and a user with a correctly configured
//! companion saw no tools at all and no explanation. A protocol the ecosystem
//! cannot open is not a stricter protocol, it is an absent one. So a host that
//! introduces itself the classic way is answered the classic way, and the
//! negotiated revision is remembered for the rest of the process.
//!
//! This is a compatibility surface, not a second implementation: both dialects
//! reach the same tools with the same arguments, and the seat behind them is
//! unchanged.

use serde_json::{Map, Value, json};

/// The revision this companion prefers, and the only stateless one it speaks.
pub const PROTOCOL_VERSION: &str = "2026-07-28";

/// Revisions accepted from a host that opens with `initialize`, newest first.
///
/// These are handshake revisions: the version is agreed once and every later
/// request is taken on that agreement rather than restating it.
pub const HANDSHAKE_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Returns every revision this companion can speak, preferred first.
#[must_use]
pub fn supported_versions() -> Vec<&'static str> {
    let mut versions = vec![PROTOCOL_VERSION];
    versions.extend_from_slice(HANDSHAKE_VERSIONS);
    versions
}

/// `_meta` key carrying a request's protocol version.
pub const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
/// `_meta` key carrying a request's client capabilities.
pub const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
/// `_meta` key carrying the server's identity on results.
pub const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

/// JSON-RPC: the method does not exist.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC: the parameters were missing or unusable.
pub const INVALID_PARAMS: i64 = -32602;
/// MCP: the requested protocol version is not implemented.
pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// A decoded JSON-RPC request.
pub struct Incoming {
    /// Request identity, absent for notifications.
    pub id: Option<Value>,
    /// Method name.
    pub method: String,
    /// Parameters, or an empty object.
    pub params: Map<String, Value>,
}

impl Incoming {
    /// Decodes one line of stdio traffic.
    ///
    /// # Errors
    ///
    /// Returns a JSON-RPC error object when the line is not a request this
    /// server can act on.
    pub fn parse(line: &str) -> Result<Self, Value> {
        let value: Value = serde_json::from_str(line)
            .map_err(|error| error_object(-32700, &format!("parse error: {error}"), None))?;
        let object = value
            .as_object()
            .ok_or_else(|| error_object(-32600, "a request must be a JSON object", None))?;
        let method = object
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| error_object(-32600, "a request must name a method", None))?
            .to_owned();
        let params = object
            .get("params")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        Ok(Self {
            id: object.get("id").cloned(),
            method,
            params,
        })
    }

    /// Checks the per-request protocol fields a stateless request carries.
    ///
    /// `negotiated` is the revision agreed during `initialize`, when the host
    /// introduced itself that way. A host that has already agreed a revision
    /// does not restate it on every request, so there is nothing to check and
    /// demanding it anyway is what made this server look broken to every
    /// standard client.
    ///
    /// # Errors
    ///
    /// Returns `-32602` when a required field is missing, and `-32022` when
    /// the version is one this companion does not implement.
    pub fn check_protocol(&self, negotiated: Option<&str>) -> Result<(), Value> {
        if negotiated.is_some() {
            return Ok(());
        }
        let meta = self
            .params
            .get("_meta")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                error_object(
                    INVALID_PARAMS,
                    "requests must carry the per-request protocol fields in _meta, or open the \
                     session with initialize",
                    None,
                )
            })?;
        let version = meta
            .get(META_PROTOCOL_VERSION)
            .and_then(Value::as_str)
            .ok_or_else(|| {
                error_object(
                    INVALID_PARAMS,
                    &format!("_meta is missing {META_PROTOCOL_VERSION}"),
                    None,
                )
            })?;
        if !meta.contains_key(META_CLIENT_CAPABILITIES) {
            return Err(error_object(
                INVALID_PARAMS,
                &format!("_meta is missing {META_CLIENT_CAPABILITIES}"),
                None,
            ));
        }
        if version != PROTOCOL_VERSION {
            return Err(error_object(
                UNSUPPORTED_PROTOCOL_VERSION,
                "Unsupported protocol version",
                Some(json!({
                    "supported": supported_versions(),
                    "requested": version,
                })),
            ));
        }
        Ok(())
    }
}

/// Chooses the revision to answer an `initialize` with.
///
/// A host names what it wants; this returns that same revision when it is one
/// we speak, and otherwise the newest handshake revision we do. Answering with
/// a revision rather than an error is what the specification asks for, and it
/// keeps a host that is merely newer than us working instead of stranded.
#[must_use]
pub fn negotiate(requested: Option<&str>) -> String {
    let fallback = HANDSHAKE_VERSIONS
        .first()
        .copied()
        .unwrap_or(PROTOCOL_VERSION);
    match requested {
        Some(version) if version == PROTOCOL_VERSION || HANDSHAKE_VERSIONS.contains(&version) => {
            version.to_owned()
        }
        _ => fallback.to_owned(),
    }
}

/// Builds the answer to `initialize`.
///
/// The instructions travel here as well as in `server/discover`. A host
/// speaking a handshake revision never calls `server/discover`, so this is the
/// only place its model is ever told how to work the seat.
#[must_use]
pub fn initialize_result(
    version: &str,
    server: &str,
    server_version: &str,
    instructions: &str,
) -> Value {
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": server, "version": server_version },
        "instructions": instructions,
    })
}

/// Wraps a result body without the stateless revision's stamps.
#[must_use]
pub fn plain_result_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Builds a JSON-RPC error object.
#[must_use]
pub fn error_object(code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({ "code": code, "message": message });
    if let Some(data) = data {
        error["data"] = data;
    }
    error
}

/// Wraps a result body in a JSON-RPC response, stamping the identity fields
/// this revision requires.
#[must_use]
pub fn result_response(id: Value, mut result: Value, server: &str, version: &str) -> Value {
    if let Some(object) = result.as_object_mut() {
        object.insert("resultType".to_owned(), json!("complete"));
        object.insert(
            "_meta".to_owned(),
            json!({ META_SERVER_INFO: { "name": server, "version": version } }),
        );
    }
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Wraps an error object in a JSON-RPC response.
#[must_use]
pub fn error_response(id: Value, error: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": error })
}

#[cfg(test)]
mod tests {
    use super::{
        INVALID_PARAMS, Incoming, PROTOCOL_VERSION, UNSUPPORTED_PROTOCOL_VERSION, result_response,
    };
    use serde_json::json;

    fn request(meta: serde_json::Value) -> Incoming {
        let line = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": { "_meta": meta },
        })
        .to_string();
        Incoming::parse(&line).expect("parses")
    }

    #[test]
    fn a_complete_request_passes_the_protocol_check() {
        let incoming = request(json!({
            "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
            "io.modelcontextprotocol/clientCapabilities": {},
        }));
        incoming.check_protocol(None).expect("valid");
        assert_eq!(incoming.method, "tools/list");
    }

    #[test]
    fn missing_protocol_fields_are_invalid_params() {
        for meta in [
            json!({}),
            json!({ "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION }),
            json!({ "io.modelcontextprotocol/clientCapabilities": {} }),
        ] {
            let error = request(meta).check_protocol(None).expect_err("rejected");
            assert_eq!(error["code"], INVALID_PARAMS);
        }
    }

    #[test]
    fn another_revision_is_refused_with_the_versions_we_speak() {
        let error = request(json!({
            "io.modelcontextprotocol/protocolVersion": "2025-11-25",
            "io.modelcontextprotocol/clientCapabilities": {},
        }))
        .check_protocol(None)
        .expect_err("rejected");
        assert_eq!(error["code"], UNSUPPORTED_PROTOCOL_VERSION);
        assert_eq!(error["data"]["supported"][0], PROTOCOL_VERSION);
        assert_eq!(error["data"]["requested"], "2025-11-25");
    }

    #[test]
    fn a_request_without_meta_at_all_is_refused() {
        let line = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }).to_string();
        let error = Incoming::parse(&line)
            .expect("parses")
            .check_protocol(None)
            .expect_err("rejected");
        assert_eq!(error["code"], INVALID_PARAMS);
    }

    #[test]
    fn a_negotiated_session_does_not_restate_its_version() {
        // The regression that made this server invisible: a host that opened
        // with initialize was still told every later request was malformed.
        let line = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }).to_string();
        Incoming::parse(&line)
            .expect("parses")
            .check_protocol(Some("2025-06-18"))
            .expect("a negotiated session needs no per-request _meta");
    }

    #[test]
    fn negotiation_answers_with_a_revision_we_speak() {
        assert_eq!(super::negotiate(Some("2025-06-18")), "2025-06-18");
        assert_eq!(super::negotiate(Some(PROTOCOL_VERSION)), PROTOCOL_VERSION);
        // Unknown or absent: answer with one we speak rather than refusing.
        assert_eq!(super::negotiate(Some("2099-01-01")), "2025-06-18");
        assert_eq!(super::negotiate(None), "2025-06-18");
    }

    #[test]
    fn initialize_carries_the_instructions_and_the_tool_capability() {
        let result = super::initialize_result("2025-06-18", "nobox-agent", "0.1.0", "how to work");
        assert_eq!(result["protocolVersion"], "2025-06-18");
        assert_eq!(result["serverInfo"]["name"], "nobox-agent");
        assert_eq!(result["instructions"], "how to work");
        // A host speaking a handshake revision never calls server/discover, so
        // this is the only place its model is told anything at all.
        assert!(result["capabilities"]["tools"].is_object());
    }

    #[test]
    fn notifications_have_no_identity() {
        let line = json!({ "jsonrpc": "2.0", "method": "notifications/cancelled" }).to_string();
        assert!(Incoming::parse(&line).expect("parses").id.is_none());
    }

    #[test]
    fn results_carry_the_result_type_and_server_identity() {
        let response = result_response(json!(1), json!({ "tools": [] }), "nobox-agent", "0.1.0");
        assert_eq!(response["result"]["resultType"], "complete");
        assert_eq!(
            response["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "nobox-agent"
        );
        assert_eq!(response["jsonrpc"], "2.0");
    }
}
