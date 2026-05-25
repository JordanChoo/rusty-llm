use serde_json::json;
use worker::*;

mod dataforseo;
mod errors;
mod providers;
mod validation;

use dataforseo::{fetch_both, fetch_llm};
use errors::json_error;
use providers::Provider;
use validation::{parse_and_validate_body, read_secret, validate_auth, LlmRequest, Location};

fn generate_request_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("getrandom failed");

    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

fn build_health_body(version: &str, secrets_ok: bool) -> serde_json::Value {
    json!({
        "status": "ok",
        "version": version,
        "providers": ["gemini", "chatgpt", "both"],
        "secrets_configured": secrets_ok
    })
}

fn handle_health(env: &Env) -> Result<Response> {
    let secrets_ok = env.secret("CSVKEY").is_ok()
        && env.secret("DATAFORSEO_LOGIN").is_ok()
        && env.secret("DATAFORSEO_PASSWORD").is_ok();

    let body = build_health_body(env!("CARGO_PKG_VERSION"), secrets_ok);
    Response::from_json(&body)
}

fn handle_health_head(env: &Env) -> Result<Response> {
    let secrets_ok = env.secret("CSVKEY").is_ok()
        && env.secret("DATAFORSEO_LOGIN").is_ok()
        && env.secret("DATAFORSEO_PASSWORD").is_ok();

    let body = build_health_body(env!("CARGO_PKG_VERSION"), secrets_ok);
    let json_str = serde_json::to_string(&body).unwrap_or_default();
    let mut resp = Response::empty()?;
    resp.headers_mut()
        .set("Content-Type", "application/json")
        .ok();
    resp.headers_mut()
        .set("Content-Length", &json_str.len().to_string())
        .ok();
    Ok(resp)
}

pub fn extract_metadata(body: &str) -> (Option<String>, Option<u64>, Option<i64>) {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body) else {
        return (None, None, None);
    };

    let model = parsed["tasks"][0]["result"][0]["model"]
        .as_str()
        .map(String::from);
    let cost = parsed["tasks"][0]["cost"]
        .as_f64()
        .map(|c| (c * 100.0).round() as u64);
    let status = parsed["tasks"][0]["status_code"].as_i64();

    (model, cost, status)
}

pub struct ExtractedMetadata {
    pub dfs_status: Option<i64>,
}

async fn attach_metadata_headers(
    response: &mut Response,
    request_id: &str,
    provider: &Provider,
    duration: u64,
    retried: bool,
) -> ExtractedMetadata {
    let body_text = response.text().await.unwrap_or_default();

    let (model, cost_cents, dfs_status) = extract_metadata(&body_text);

    let headers = Headers::new();
    headers.set("Content-Type", "application/json").ok();
    headers.set("X-RustyLLM-Request-Id", request_id).ok();
    headers.set("X-RustyLLM-Provider", provider.name()).ok();
    if let Some(m) = &model {
        headers.set("X-RustyLLM-Model", m).ok();
    }
    headers
        .set("X-RustyLLM-Duration-Ms", &duration.to_string())
        .ok();
    if let Some(c) = cost_cents {
        headers
            .set("X-RustyLLM-DFS-Cost-Cents", &c.to_string())
            .ok();
    }
    if let Some(s) = dfs_status {
        headers.set("X-RustyLLM-DFS-Status", &s.to_string()).ok();
    }
    headers
        .set("X-RustyLLM-Retried", if retried { "true" } else { "false" })
        .ok();

    *response = Response::from_body(ResponseBody::Body(body_text.into_bytes()))
        .unwrap()
        .with_headers(headers)
        .with_status(200);

    ExtractedMetadata { dfs_status }
}

fn emit_log(
    request_id: &str,
    request: &LlmRequest,
    duration_ms: u64,
    status: u16,
    dfs_status: Option<i64>,
    retried: bool,
) {
    let location_str = match &request.location {
        Location::Code(c) => c.to_string(),
        Location::Name(n) => n.clone(),
    };

    console_log!(
        "{{\"req_id\":\"{}\",\"provider\":\"{}\",\"keyword_len\":{},\"location\":\"{}\",\"duration_ms\":{},\"status\":{},\"dfs_status\":{},\"retried\":{}}}",
        request_id,
        request.provider.name(),
        request.keyword.len(),
        location_str,
        duration_ms,
        status,
        dfs_status.map_or("null".to_string(), |s| s.to_string()),
        retried
    );
}

async fn handle_llm(mut req: Request, env: Env, request_id: &str) -> Result<Response, Response> {
    let start = Date::now().as_millis();

    let url = req
        .url()
        .map_err(|_| json_error(500, "Failed to parse URL", "internal_error", request_id))?;
    validate_auth(&url, &env, request_id)?;

    let llm_request = parse_and_validate_body(&mut req, request_id).await?;

    let login = read_secret(&env, "DATAFORSEO_LOGIN", request_id)?;
    let password = read_secret(&env, "DATAFORSEO_PASSWORD", request_id)?;

    let (mut response, retried) = match llm_request.provider {
        Provider::Both => fetch_both(&llm_request, &login, &password, request_id).await?,
        _ => {
            let result = fetch_llm(&llm_request, &login, &password, request_id).await?;
            (result.response, result.retried)
        }
    };

    let duration = Date::now().as_millis() - start;

    let dfs_status = if !matches!(llm_request.provider, Provider::Both) {
        let extracted = attach_metadata_headers(
            &mut response,
            request_id,
            &llm_request.provider,
            duration,
            retried,
        )
        .await;
        extracted.dfs_status
    } else {
        response
            .headers_mut()
            .set("X-RustyLLM-Request-Id", request_id)
            .ok();
        response
            .headers_mut()
            .set("X-RustyLLM-Provider", "both")
            .ok();
        response
            .headers_mut()
            .set("X-RustyLLM-Duration-Ms", &duration.to_string())
            .ok();
        response
            .headers_mut()
            .set("X-RustyLLM-Retried", if retried { "true" } else { "false" })
            .ok();
        None
    };

    emit_log(
        request_id,
        &llm_request,
        duration,
        response.status_code(),
        dfs_status,
        retried,
    );

    Ok(response)
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();

    let path = req.path();
    let method = req.method();

    match (method, path.as_str()) {
        (Method::Get, "/v1/health") => handle_health(&env),
        (Method::Head, "/v1/health") => handle_health_head(&env),
        (Method::Post, "/v1/llm") => {
            let request_id = generate_request_id();
            Ok(handle_llm(req, env, &request_id)
                .await
                .unwrap_or_else(|e| e))
        }
        (_, "/v1/health") | (_, "/v1/llm") => Ok(json_error(
            405,
            "Method not allowed",
            "method_not_allowed",
            "",
        )),
        _ => Ok(json_error(404, "Not found", "not_found", "")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_is_valid_uuid_v4_format() {
        let id = generate_request_id();
        let re = regex::Regex::new(
            r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$",
        )
        .unwrap();
        assert!(re.is_match(&id), "ID '{}' is not valid UUID v4", id);
    }

    #[test]
    fn request_id_is_lowercase() {
        let id = generate_request_id();
        assert_eq!(id, id.to_lowercase());
    }

    #[test]
    fn request_id_is_36_chars() {
        let id = generate_request_id();
        assert_eq!(id.len(), 36);
    }

    #[test]
    fn request_ids_are_unique() {
        let id1 = generate_request_id();
        let id2 = generate_request_id();
        assert_ne!(id1, id2);
    }

    #[test]
    fn request_id_version_nibble_is_4() {
        let id = generate_request_id();
        assert_eq!(id.chars().nth(14).unwrap(), '4');
    }

    #[test]
    fn health_body_structure() {
        let body = build_health_body("0.1.0", true);
        assert_eq!(body["status"], "ok");
        assert_eq!(body["version"], "0.1.0");
        assert_eq!(body["secrets_configured"], true);
        let providers = body["providers"].as_array().unwrap();
        assert_eq!(providers.len(), 3);
    }

    #[test]
    fn health_body_secrets_false() {
        let body = build_health_body("0.1.0", false);
        assert_eq!(body["secrets_configured"], false);
    }

    #[test]
    fn health_body_providers_order() {
        let body = build_health_body("0.1.0", true);
        let providers: Vec<&str> = body["providers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(providers, vec!["gemini", "chatgpt", "both"]);
    }

    #[test]
    fn extract_metadata_valid_response() {
        let body = r#"{"tasks":[{"status_code":20000,"cost":0.04,"result":[{"model":"gemini-2.0-flash"}]}]}"#;
        let (model, cost, status) = extract_metadata(body);
        assert_eq!(model.as_deref(), Some("gemini-2.0-flash"));
        assert_eq!(cost, Some(4));
        assert_eq!(status, Some(20000));
    }

    #[test]
    fn extract_metadata_empty_string() {
        let (model, cost, status) = extract_metadata("");
        assert_eq!(model, None);
        assert_eq!(cost, None);
        assert_eq!(status, None);
    }

    #[test]
    fn extract_metadata_invalid_json() {
        let (model, cost, status) = extract_metadata("{not valid json");
        assert_eq!(model, None);
        assert_eq!(cost, None);
        assert_eq!(status, None);
    }

    #[test]
    fn extract_metadata_missing_result() {
        let body = r#"{"tasks":[{"status_code":20000,"cost":0.04}]}"#;
        let (model, cost, status) = extract_metadata(body);
        assert_eq!(model, None);
        assert_eq!(cost, Some(4));
        assert_eq!(status, Some(20000));
    }

    #[test]
    fn extract_metadata_null_model() {
        let body = r#"{"tasks":[{"status_code":20000,"cost":0.04,"result":[{"model":null}]}]}"#;
        let (model, cost, status) = extract_metadata(body);
        assert_eq!(model, None);
        assert_eq!(cost, Some(4));
        assert_eq!(status, Some(20000));
    }

    #[test]
    fn extract_metadata_empty_tasks() {
        let body = r#"{"tasks":[]}"#;
        let (model, cost, status) = extract_metadata(body);
        assert_eq!(model, None);
        assert_eq!(cost, None);
        assert_eq!(status, None);
    }

    #[test]
    fn extract_metadata_chatgpt_model() {
        let body = r#"{"tasks":[{"status_code":20000,"cost":0.04,"result":[{"model":"gpt-4o"}]}]}"#;
        let (model, _cost, _status) = extract_metadata(body);
        assert_eq!(model.as_deref(), Some("gpt-4o"));
    }
}
