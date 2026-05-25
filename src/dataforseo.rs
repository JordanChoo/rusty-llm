use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::time::Duration;
use worker::{Fetch, Headers, Method, Request, RequestInit, Response};

use crate::errors::{dataforseo_error, json_error};
use crate::providers::Provider;
use crate::validation::{LlmRequest, Location};

pub struct FetchResult {
    pub response: Response,
    pub retried: bool,
}

#[derive(Debug)]
pub struct ProviderOutcome {
    pub status: &'static str,
    pub duration_ms: u64,
    pub model: Option<String>,
    pub dfs_cost_cents: Option<u64>,
    pub response_body: Option<Value>,
    pub error: Option<String>,
    pub code: Option<String>,
    pub dataforseo_status: Option<u16>,
    pub dataforseo_body: Option<String>,
    pub retried: bool,
}

pub fn build_dfs_task(request: &LlmRequest, request_id: &str) -> Value {
    let mut task = json!({
        "keyword": request.keyword,
        "language_code": request.language,
    });

    match &request.location {
        Location::Code(n) => {
            task["location_code"] = json!(n);
        }
        Location::Name(s) => {
            task["location_name"] = json!(s);
        }
    }

    if request.provider.supports_force_web_search() && request.force_web_search {
        task["force_web_search"] = json!(true);
    }

    task["tag"] = json!(request.tag.as_deref().unwrap_or(request_id));

    task
}

pub fn build_auth_header(login: &str, password: &str) -> String {
    let credentials = format!("{}:{}", login, password);
    format!("Basic {}", STANDARD.encode(credentials.as_bytes()))
}

pub fn wrap_task_array(task: Value) -> String {
    serde_json::to_string(&json!([task])).unwrap()
}

pub fn build_envelope(
    gemini: ProviderOutcome,
    chatgpt: ProviderOutcome,
    request_id: &str,
    total_ms: u64,
) -> (u16, Value) {
    let gemini_ok = gemini.status == "ok";
    let chatgpt_ok = chatgpt.status == "ok";

    let status_code = match (gemini_ok, chatgpt_ok) {
        (true, true) => 200,
        (false, false) => 502,
        _ => 207,
    };

    fn provider_json(outcome: ProviderOutcome) -> Value {
        let mut obj = json!({
            "status": outcome.status,
            "duration_ms": outcome.duration_ms,
            "retried": outcome.retried,
        });
        if let Some(model) = outcome.model {
            obj["model"] = json!(model);
        }
        if let Some(cost) = outcome.dfs_cost_cents {
            obj["dfs_cost_cents"] = json!(cost);
        }
        if let Some(body) = outcome.response_body {
            obj["response"] = body;
        }
        if let Some(err) = outcome.error {
            obj["error"] = json!(err);
        }
        if let Some(code) = outcome.code {
            obj["code"] = json!(code);
        }
        if let Some(dfs_status) = outcome.dataforseo_status {
            obj["dataforseo_status"] = json!(dfs_status);
        }
        if let Some(dfs_body) = outcome.dataforseo_body {
            obj["dataforseo_body"] = json!(dfs_body);
        }
        obj
    }

    let envelope = json!({
        "request_id": request_id,
        "provider": "both",
        "duration_ms": total_ms,
        "gemini": provider_json(gemini),
        "chatgpt": provider_json(chatgpt),
    });

    (status_code, envelope)
}

pub async fn fetch_llm(
    request: &LlmRequest,
    login: &str,
    password: &str,
    request_id: &str,
) -> Result<FetchResult, Response> {
    let url = request.provider.url();
    let task = build_dfs_task(request, request_id);
    let body = wrap_task_array(task);
    let auth = build_auth_header(login, password);

    match send_request(url, &body, &auth).await {
        Ok(resp) if resp.status_code() == 429 || resp.status_code() == 503 => {
            retry_once(url, &body, &auth, request_id).await
        }
        Ok(resp) if resp.status_code() >= 200 && resp.status_code() < 300 => Ok(FetchResult {
            response: resp,
            retried: false,
        }),
        Ok(mut resp) => {
            let status = resp.status_code();
            let resp_body = resp.text().await.unwrap_or_default();
            Err(dataforseo_error(
                status,
                &resp_body,
                "dataforseo_error",
                request_id,
            ))
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("abort") || msg.contains("timeout") || msg.contains("Timeout") {
                Err(json_error(
                    504,
                    "DataForSEO request timed out",
                    "dataforseo_timeout",
                    request_id,
                ))
            } else {
                Err(dataforseo_error(0, &msg, "dataforseo_error", request_id))
            }
        }
    }
}

async fn send_request(url: &str, body: &str, auth: &str) -> Result<Response, worker::Error> {
    let headers = Headers::new();
    let _ = headers.set("Content-Type", "application/json");
    let _ = headers.set("Authorization", auth);

    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(body.into()));

    let req = Request::new_with_init(url, &init)?;
    Fetch::Request(req).send().await
}

async fn retry_once(
    url: &str,
    body: &str,
    auth: &str,
    request_id: &str,
) -> Result<FetchResult, Response> {
    worker::Delay::from(Duration::from_secs(1)).await;

    match send_request(url, body, auth).await {
        Ok(resp) if resp.status_code() >= 200 && resp.status_code() < 300 => Ok(FetchResult {
            response: resp,
            retried: true,
        }),
        Ok(mut resp) => {
            let status = resp.status_code();
            let resp_body = resp.text().await.unwrap_or_default();
            Err(dataforseo_error(
                status,
                &resp_body,
                "dataforseo_error",
                request_id,
            ))
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("abort") || msg.contains("timeout") || msg.contains("Timeout") {
                Err(json_error(
                    504,
                    "DataForSEO request timed out after retry",
                    "dataforseo_timeout",
                    request_id,
                ))
            } else {
                Err(dataforseo_error(0, &msg, "dataforseo_error", request_id))
            }
        }
    }
}

pub async fn fetch_both(
    request: &LlmRequest,
    login: &str,
    password: &str,
    request_id: &str,
) -> Result<(Response, bool), Response> {
    let gemini_request = LlmRequest {
        provider: Provider::Gemini,
        ..request.clone()
    };
    let chatgpt_request = LlmRequest {
        provider: Provider::ChatGpt,
        force_web_search: request.force_web_search,
        ..request.clone()
    };

    let start = web_time_ms();
    let (gemini_outcome, chatgpt_outcome) = futures::join!(
        fetch_single_for_both(&gemini_request, login, password, request_id),
        fetch_single_for_both(&chatgpt_request, login, password, request_id),
    );
    let total_ms = web_time_ms() - start;

    let either_retried = gemini_outcome.retried || chatgpt_outcome.retried;
    let (status_code, envelope) =
        build_envelope(gemini_outcome, chatgpt_outcome, request_id, total_ms);

    let mut resp = Response::from_json(&envelope)
        .unwrap_or_else(|_| Response::error("Internal Server Error", 500).unwrap());
    let _ = resp.headers_mut().set("Content-Type", "application/json");
    let _ = resp.headers_mut().set("X-RustyLLM-Request-Id", request_id);
    Ok((resp.with_status(status_code), either_retried))
}

async fn fetch_single_for_both(
    request: &LlmRequest,
    login: &str,
    password: &str,
    request_id: &str,
) -> ProviderOutcome {
    let start = web_time_ms();
    match fetch_llm(request, login, password, request_id).await {
        Ok(mut result) => {
            let duration_ms = web_time_ms() - start;
            let body_text = result.response.text().await.unwrap_or_default();
            let body_json: Value = serde_json::from_str(&body_text).unwrap_or(json!(null));

            let model = body_json["tasks"][0]["result"][0]["model"]
                .as_str()
                .map(|s| s.to_string());
            let cost = body_json["cost"]
                .as_f64()
                .map(|c| (c * 100.0).round() as u64);

            ProviderOutcome {
                status: "ok",
                duration_ms,
                model,
                dfs_cost_cents: cost,
                response_body: Some(body_json),
                error: None,
                code: None,
                dataforseo_status: None,
                dataforseo_body: None,
                retried: result.retried,
            }
        }
        Err(mut err_resp) => {
            let duration_ms = web_time_ms() - start;
            let err_text = err_resp.text().await.unwrap_or_default();
            let err_json: Value = serde_json::from_str(&err_text).unwrap_or(json!(null));

            ProviderOutcome {
                status: "error",
                duration_ms,
                model: None,
                dfs_cost_cents: None,
                response_body: None,
                error: err_json["error"].as_str().map(|s| s.to_string()),
                code: err_json["code"].as_str().map(|s| s.to_string()),
                dataforseo_status: err_json["dataforseo_status"].as_u64().map(|n| n as u16),
                dataforseo_body: err_json["dataforseo_body"].as_str().map(|s| s.to_string()),
                retried: false,
            }
        }
    }
}

fn web_time_ms() -> u64 {
    worker::Date::now().as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Provider;
    use crate::validation::{LlmRequest, Location};

    fn make_request(provider: Provider, keyword: &str, location: Location) -> LlmRequest {
        LlmRequest {
            provider,
            keyword: keyword.to_string(),
            location,
            language: "en".to_string(),
            force_web_search: false,
            tag: None,
        }
    }

    // --- build_dfs_task ---

    #[test]
    fn build_task_basic_gemini() {
        let req = make_request(Provider::Gemini, "test query", Location::Code(2840));
        let task = build_dfs_task(&req, "req-123");
        assert_eq!(task["keyword"], "test query");
        assert_eq!(task["language_code"], "en");
        assert_eq!(task["location_code"], 2840);
        assert_eq!(task["tag"], "req-123");
    }

    #[test]
    fn build_task_location_code() {
        let req = make_request(Provider::Gemini, "test", Location::Code(1234));
        let task = build_dfs_task(&req, "id");
        assert_eq!(task["location_code"], 1234);
        assert!(task.get("location_name").is_none() || task["location_name"].is_null());
    }

    #[test]
    fn build_task_location_name() {
        let req = make_request(
            Provider::Gemini,
            "test",
            Location::Name("United States".to_string()),
        );
        let task = build_dfs_task(&req, "id");
        assert_eq!(task["location_name"], "United States");
        assert!(task.get("location_code").is_none() || task["location_code"].is_null());
    }

    #[test]
    fn build_task_chatgpt_with_force_web_search() {
        let mut req = make_request(Provider::ChatGpt, "test", Location::Code(2840));
        req.force_web_search = true;
        let task = build_dfs_task(&req, "id");
        assert_eq!(task["force_web_search"], true);
    }

    #[test]
    fn build_task_chatgpt_without_force_web_search() {
        let req = make_request(Provider::ChatGpt, "test", Location::Code(2840));
        let task = build_dfs_task(&req, "id");
        assert!(task.get("force_web_search").is_none() || task["force_web_search"].is_null());
    }

    #[test]
    fn build_task_gemini_ignores_force_web_search() {
        let mut req = make_request(Provider::Gemini, "test", Location::Code(2840));
        req.force_web_search = true;
        let task = build_dfs_task(&req, "id");
        assert!(task.get("force_web_search").is_none() || task["force_web_search"].is_null());
    }

    #[test]
    fn build_task_custom_tag() {
        let mut req = make_request(Provider::Gemini, "test", Location::Code(2840));
        req.tag = Some("custom-tag-123".to_string());
        let task = build_dfs_task(&req, "req-id");
        assert_eq!(task["tag"], "custom-tag-123");
    }

    #[test]
    fn build_task_no_tag_uses_request_id() {
        let req = make_request(Provider::Gemini, "test", Location::Code(2840));
        let task = build_dfs_task(&req, "my-request-id");
        assert_eq!(task["tag"], "my-request-id");
    }

    #[test]
    fn build_task_custom_language() {
        let mut req = make_request(Provider::Gemini, "test", Location::Code(2840));
        req.language = "fr".to_string();
        let task = build_dfs_task(&req, "id");
        assert_eq!(task["language_code"], "fr");
    }

    #[test]
    fn build_task_keyword_preserved_verbatim() {
        let req = make_request(
            Provider::Gemini,
            "hello world + stuff",
            Location::Code(2840),
        );
        let task = build_dfs_task(&req, "id");
        assert_eq!(task["keyword"], "hello world + stuff");
    }

    // --- build_auth_header ---

    #[test]
    fn auth_header_basic_format() {
        let header = build_auth_header("user@test.com", "mypassword");
        assert!(header.starts_with("Basic "));
    }

    #[test]
    fn auth_header_correct_base64() {
        let header = build_auth_header("user", "pass");
        assert_eq!(header, "Basic dXNlcjpwYXNz");
    }

    #[test]
    fn auth_header_special_chars() {
        let header = build_auth_header("email@domain.com", "p@ss:word!");
        let encoded = header.strip_prefix("Basic ").unwrap();
        let decoded = String::from_utf8(STANDARD.decode(encoded).unwrap()).unwrap();
        assert_eq!(decoded, "email@domain.com:p@ss:word!");
    }

    // --- wrap_task_array ---

    #[test]
    fn wrap_task_produces_array() {
        let task = json!({"keyword": "test"});
        let result = wrap_task_array(task);
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 1);
    }

    #[test]
    fn wrap_task_preserves_content() {
        let task = json!({"keyword": "hello", "location_code": 2840});
        let result = wrap_task_array(task);
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed[0]["keyword"], "hello");
        assert_eq!(parsed[0]["location_code"], 2840);
    }

    // --- build_envelope ---

    fn ok_outcome(model: &str, ms: u64) -> ProviderOutcome {
        ProviderOutcome {
            status: "ok",
            duration_ms: ms,
            model: Some(model.to_string()),
            dfs_cost_cents: Some(40),
            response_body: Some(json!({"tasks": []})),
            error: None,
            code: None,
            dataforseo_status: None,
            dataforseo_body: None,
            retried: false,
        }
    }

    fn err_outcome(ms: u64) -> ProviderOutcome {
        ProviderOutcome {
            status: "error",
            duration_ms: ms,
            model: None,
            dfs_cost_cents: None,
            response_body: None,
            error: Some("Upstream failed".to_string()),
            code: Some("dataforseo_error".to_string()),
            dataforseo_status: Some(500),
            dataforseo_body: Some("Internal error".to_string()),
            retried: false,
        }
    }

    #[test]
    fn envelope_both_ok_returns_200() {
        let (status, envelope) = build_envelope(
            ok_outcome("gemini-2.0", 4000),
            ok_outcome("gpt-4o", 8000),
            "req-1",
            8000,
        );
        assert_eq!(status, 200);
        assert_eq!(envelope["gemini"]["status"], "ok");
        assert_eq!(envelope["chatgpt"]["status"], "ok");
        assert_eq!(envelope["request_id"], "req-1");
    }

    #[test]
    fn envelope_one_fails_returns_207() {
        let (status, envelope) = build_envelope(
            ok_outcome("gemini-2.0", 4000),
            err_outcome(12000),
            "req-2",
            12000,
        );
        assert_eq!(status, 207);
        assert_eq!(envelope["gemini"]["status"], "ok");
        assert_eq!(envelope["chatgpt"]["status"], "error");
    }

    #[test]
    fn envelope_both_fail_returns_502() {
        let (status, _) = build_envelope(err_outcome(5000), err_outcome(8000), "req-3", 8000);
        assert_eq!(status, 502);
    }

    #[test]
    fn envelope_includes_per_provider_timing() {
        let (_, envelope) = build_envelope(
            ok_outcome("gemini-2.0", 3500),
            ok_outcome("gpt-4o", 7200),
            "req-4",
            7200,
        );
        assert_eq!(envelope["gemini"]["duration_ms"], 3500);
        assert_eq!(envelope["chatgpt"]["duration_ms"], 7200);
        assert_eq!(envelope["duration_ms"], 7200);
    }

    #[test]
    fn envelope_error_includes_details() {
        let (_, envelope) = build_envelope(
            ok_outcome("gemini-2.0", 4000),
            err_outcome(12000),
            "req-5",
            12000,
        );
        assert_eq!(envelope["chatgpt"]["dataforseo_status"], 500);
        assert!(
            envelope["chatgpt"]["dataforseo_body"]
                .as_str()
                .unwrap()
                .len()
                > 0
        );
    }
}
