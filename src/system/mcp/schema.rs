//! Per-route typed MCP tool input schemas (v1 roadmap #13).
//!
//! Builds each tool's `inputSchema` from what riz already knows — the route
//! templates type `pathParams`, and the optional `[function.X.mcp]` block
//! types query params and the body. Precise schemas measurably improve LLM
//! tool-calling accuracy; the generic envelope remains the fallback so a
//! schema-less function behaves exactly as before.

use crate::config::McpToolConfig;

/// Extract `{name}` / `{name+}` path-parameter names from a route's path
/// portion, in declaration order.
pub(super) fn path_param_names(path: &str) -> Vec<String> {
    path.trim_start_matches('/')
        .split('/')
        .filter_map(|seg| {
            seg.strip_prefix('{').and_then(|s| {
                s.strip_suffix("+}")
                    .or_else(|| s.strip_suffix('}'))
                    .map(str::to_string)
            })
        })
        .collect()
}

/// Path params for a "METHOD /path" route entry.
fn route_path_params(route: &str) -> Vec<String> {
    match route.split_once(' ') {
        Some((_, path)) => path_param_names(path),
        None => Vec::new(),
    }
}

/// Build the tool's `inputSchema` for a function with the given
/// "METHOD /path" routes and optional MCP tuning block.
///
/// Layering on top of the generic envelope:
/// - route templates with `{params}` → typed `pathParams` (required when the
///   param appears in every declared route, so single-route functions get a
///   hard requirement);
/// - `[function.X.mcp.query]` → typed `queryParams` properties + required;
/// - `[function.X.mcp]` `body` → verbatim JSON Schema replacing the generic
///   string body;
/// - multiple routes → a `route` enum so the agent picks a declared one.
pub(super) fn tool_input_schema(
    routes: &[String],
    mcp: Option<&McpToolConfig>,
) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut top_required: Vec<String> = Vec::new();

    properties.insert("route".into(), route_property(routes));
    properties.insert("body".into(), body_property(mcp));
    properties.insert(
        "headers".into(),
        serde_json::json!({"type": "object", "additionalProperties": {"type": "string"}}),
    );

    let (qp, qp_required) = query_params_property(mcp);
    if qp_required {
        top_required.push("queryParams".into());
    }
    properties.insert("queryParams".into(), qp);

    let (pp, pp_required) = path_params_property(routes);
    if pp_required {
        top_required.push("pathParams".into());
    }
    properties.insert("pathParams".into(), pp);

    properties.insert(
        "isBase64Encoded".into(),
        serde_json::json!({"type": "boolean", "default": false}),
    );

    let mut schema = serde_json::Map::new();
    schema.insert("type".into(), serde_json::json!("object"));
    schema.insert("properties".into(), serde_json::Value::Object(properties));
    if !top_required.is_empty() {
        schema.insert("required".into(), serde_json::json!(top_required));
    }
    serde_json::Value::Object(schema)
}

/// `route` selector — enum over declared routes when there's a choice.
fn route_property(routes: &[String]) -> serde_json::Value {
    let mut prop = serde_json::Map::new();
    prop.insert("type".into(), serde_json::json!("string"));
    prop.insert(
        "description".into(),
        serde_json::json!(
            "Optional \"METHOD /path\" selector when the function declares multiple routes. Omit to use the first declared route."
        ),
    );
    if routes.len() > 1 {
        prop.insert("enum".into(), serde_json::json!(routes));
    }
    serde_json::Value::Object(prop)
}

/// `body` — declared schema verbatim, else the generic string.
fn body_property(mcp: Option<&McpToolConfig>) -> serde_json::Value {
    match mcp.and_then(|m| m.body.clone()) {
        Some(schema) => schema,
        None => serde_json::json!({
            "type": "string",
            "description": "Request body. Set isBase64Encoded:true for binary."
        }),
    }
}

/// `queryParams` — typed properties from the mcp.query block; undeclared
/// params stay accepted (HTTP query strings are open-world). The bool says
/// whether the object carries required params (which makes `queryParams`
/// itself required at the top level).
fn query_params_property(mcp: Option<&McpToolConfig>) -> (serde_json::Value, bool) {
    let mut qp = serde_json::Map::new();
    qp.insert("type".into(), serde_json::json!("object"));
    qp.insert(
        "additionalProperties".into(),
        serde_json::json!({"type": "string"}),
    );
    let Some(m) = mcp.filter(|m| !m.query.is_empty()) else {
        return (serde_json::Value::Object(qp), false);
    };
    let mut qprops = serde_json::Map::new();
    let mut qrequired: Vec<String> = Vec::new();
    for (pname, spec) in &m.query {
        let mut prop = serde_json::Map::new();
        prop.insert("type".into(), serde_json::json!(spec.kind));
        if let Some(d) = &spec.description {
            prop.insert("description".into(), serde_json::json!(d));
        }
        qprops.insert(pname.clone(), serde_json::Value::Object(prop));
        if spec.required {
            qrequired.push(pname.clone());
        }
    }
    qp.insert("properties".into(), serde_json::Value::Object(qprops));
    let has_required = !qrequired.is_empty();
    if has_required {
        qp.insert("required".into(), serde_json::json!(qrequired));
    }
    (serde_json::Value::Object(qp), has_required)
}

/// `pathParams` — typed from the route templates. A param is hard-required
/// only when every declared route carries it (multi-route functions can't
/// require a param that only one route uses). The bool says whether any
/// param is required everywhere (which makes `pathParams` itself required).
fn path_params_property(routes: &[String]) -> (serde_json::Value, bool) {
    let per_route: Vec<Vec<String>> = routes.iter().map(|r| route_path_params(r)).collect();
    let mut all_params: Vec<String> = Vec::new();
    for params in &per_route {
        for p in params {
            if !all_params.contains(p) {
                all_params.push(p.clone());
            }
        }
    }
    if all_params.is_empty() {
        return (
            serde_json::json!({"type": "object", "additionalProperties": {"type": "string"}}),
            false,
        );
    }
    let mut pprops = serde_json::Map::new();
    for p in &all_params {
        pprops.insert(
            p.clone(),
            serde_json::json!({
                "type": "string",
                "description": format!("Path parameter `{{{p}}}` from the route template")
            }),
        );
    }
    let required_everywhere: Vec<String> = all_params
        .iter()
        .filter(|p| per_route.iter().all(|params| params.contains(p)))
        .cloned()
        .collect();
    let mut pp = serde_json::Map::new();
    pp.insert("type".into(), serde_json::json!("object"));
    pp.insert("properties".into(), serde_json::Value::Object(pprops));
    pp.insert("additionalProperties".into(), serde_json::json!(false));
    let has_required = !required_everywhere.is_empty();
    if has_required {
        pp.insert("required".into(), serde_json::json!(required_everywhere));
    }
    (serde_json::Value::Object(pp), has_required)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_param_names_extracts_single_and_greedy() {
        assert_eq!(path_param_names("/accounts/{id}"), vec!["id"]);
        assert_eq!(path_param_names("/files/{key+}"), vec!["key"]);
        assert_eq!(
            path_param_names("/a/{x}/b/{y+}"),
            vec!["x".to_string(), "y".to_string()]
        );
        assert!(path_param_names("/plain/route").is_empty());
    }

    #[test]
    fn schema_requires_param_present_in_every_route() {
        let routes = vec![
            "GET /orders/{id}".to_string(),
            "DELETE /orders/{id}".to_string(),
        ];
        let s = tool_input_schema(&routes, None);
        let req = s["properties"]["pathParams"]["required"]
            .as_array()
            .unwrap();
        assert!(req.iter().any(|v| v == "id"));
    }

    #[test]
    fn schema_does_not_require_param_missing_from_some_route() {
        let routes = vec!["GET /orders/{id}".to_string(), "POST /orders".to_string()];
        let s = tool_input_schema(&routes, None);
        // `id` is typed but not required — POST /orders doesn't carry it.
        assert_eq!(
            s["properties"]["pathParams"]["properties"]["id"]["type"],
            "string"
        );
        assert!(s["properties"]["pathParams"].get("required").is_none());
        assert!(s.get("required").is_none());
    }

    #[test]
    fn generic_shape_when_no_params_and_no_mcp() {
        let routes = vec!["GET /echo".to_string()];
        let s = tool_input_schema(&routes, None);
        assert!(s["properties"]["pathParams"]["additionalProperties"].is_object());
        assert!(s.get("required").is_none());
    }
}
