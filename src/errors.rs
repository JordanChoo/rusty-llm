use serde_json::{json, Value};
use worker::Response;

pub fn truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub fn build_error_json(error: &str, code: &str, request_id: &str) -> Value {
    json!({
        "request_id": request_id,
        "error": error,
        "code": code,
    })
}

pub fn build_dfs_error_json(
    dfs_status: u16,
    dfs_body: &str,
    code: &str,
    request_id: &str,
) -> Value {
    json!({
        "request_id": request_id,
        "error": "DataForSEO returned non-success status",
        "code": code,
        "dataforseo_status": dfs_status,
        "dataforseo_body": truncate(dfs_body, 4096),
    })
}

pub fn json_error(status: u16, error: &str, code: &str, request_id: &str) -> Response {
    let body = build_error_json(error, code, request_id);
    let mut resp = Response::from_json(&body)
        .unwrap_or_else(|_| Response::error("Internal Server Error", 500).unwrap());
    let _ = resp.headers_mut().set("Content-Type", "application/json");
    let _ = resp.headers_mut().set("X-RustyLLM-Request-Id", request_id);
    resp.with_status(status)
}

pub fn dataforseo_error(dfs_status: u16, dfs_body: &str, code: &str, request_id: &str) -> Response {
    let body = build_dfs_error_json(dfs_status, dfs_body, code, request_id);
    let mut resp = Response::from_json(&body)
        .unwrap_or_else(|_| Response::error("Internal Server Error", 500).unwrap());
    let _ = resp.headers_mut().set("Content-Type", "application/json");
    let _ = resp.headers_mut().set("X-RustyLLM-Request-Id", request_id);
    resp.with_status(502)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_under_limit_returns_full_string() {
        assert_eq!(truncate("hello world", 100), "hello world");
    }

    #[test]
    fn truncate_at_exact_limit_returns_full_string() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_over_limit_ascii() {
        assert_eq!(truncate("hello world", 5), "hello");
    }

    #[test]
    fn truncate_empty_string() {
        assert_eq!(truncate("", 10), "");
    }

    #[test]
    fn truncate_zero_limit() {
        assert_eq!(truncate("hello", 0), "");
    }

    #[test]
    fn truncate_multibyte_does_not_split_char() {
        let s = "é";
        let result = truncate(s, 1);
        assert!(result.is_empty(), "Should not split multi-byte char");
    }

    #[test]
    fn truncate_multibyte_keeps_complete_chars() {
        let s = "aé";
        assert_eq!(truncate(s, 3), "aé");
        assert_eq!(truncate(s, 2), "a");
        assert_eq!(truncate(s, 1), "a");
    }

    #[test]
    fn truncate_emoji() {
        let s = "hi🦀";
        assert_eq!(truncate(s, 6), "hi🦀");
        assert_eq!(truncate(s, 5), "hi");
        assert_eq!(truncate(s, 2), "hi");
    }

    #[test]
    fn build_error_json_structure() {
        let json = build_error_json("Not found", "not_found", "req-123");
        assert_eq!(json["error"], "Not found");
        assert_eq!(json["code"], "not_found");
        assert_eq!(json["request_id"], "req-123");
        assert_eq!(json.as_object().unwrap().len(), 3);
    }

    #[test]
    fn build_error_json_empty_request_id() {
        let json = build_error_json("err", "code", "");
        assert_eq!(json["request_id"], "");
    }

    #[test]
    fn build_error_json_special_chars_in_message() {
        let json = build_error_json("Error: \"bad\" <input>", "err", "id");
        assert_eq!(json["error"].as_str().unwrap(), "Error: \"bad\" <input>");
    }

    #[test]
    fn build_dfs_error_json_structure() {
        let json = build_dfs_error_json(403, "Forbidden", "dataforseo_error", "req-456");
        assert_eq!(json["dataforseo_status"], 403);
        assert_eq!(json["dataforseo_body"], "Forbidden");
        assert_eq!(json["code"], "dataforseo_error");
        assert_eq!(json["request_id"], "req-456");
        assert!(json["error"].as_str().unwrap().len() > 0);
    }

    #[test]
    fn build_dfs_error_json_truncates_large_body() {
        let large_body = "x".repeat(5000);
        let json = build_dfs_error_json(500, &large_body, "dataforseo_error", "req");
        let body_str = json["dataforseo_body"].as_str().unwrap();
        assert!(body_str.len() <= 4096, "Body should be truncated to 4KB");
    }

    #[test]
    fn build_dfs_error_json_status_zero_for_network_error() {
        let json = build_dfs_error_json(0, "", "dataforseo_error", "req");
        assert_eq!(json["dataforseo_status"], 0);
    }
}
