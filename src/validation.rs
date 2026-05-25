use serde_json::Value;
use worker::{Env, Request, Response, Url};

use crate::errors::json_error;
use crate::providers::Provider;

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub provider: Provider,
    pub keyword: String,
    pub location: Location,
    pub language: String,
    pub force_web_search: bool,
    pub tag: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Location {
    Code(i64),
    Name(String),
}

pub fn sanitize_keyword(raw: &str) -> String {
    let stripped: String = raw
        .chars()
        .filter(|c| !c.is_ascii_control() || *c == ' ' || *c == '\t' || *c == '\n' || *c == '\r')
        .collect();

    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut acc: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

pub fn validate_fields(body: &Value) -> Result<LlmRequest, (u16, &'static str, &'static str)> {
    let provider = match body.get("provider") {
        None => return Err((400, "Missing required field: provider", "missing_provider")),
        Some(v) => match v.as_str() {
            None => return Err((400, "Field 'provider' must be a string", "invalid_provider")),
            Some(s) => match Provider::from_str(s) {
                None => {
                    return Err((
                        400,
                        "Invalid provider. Must be 'gemini', 'chatgpt', or 'both'",
                        "invalid_provider",
                    ))
                }
                Some(p) => p,
            },
        },
    };

    let keyword = match body.get("keyword") {
        None => return Err((400, "Missing required field: keyword", "missing_keyword")),
        Some(v) => match v.as_str() {
            None => return Err((400, "Field 'keyword' must be a string", "missing_keyword")),
            Some(s) => {
                let sanitized = sanitize_keyword(s);
                if sanitized.is_empty() {
                    return Err((
                        400,
                        "Keyword is empty after sanitization",
                        "invalid_keyword",
                    ));
                }
                if sanitized.len() > 2000 {
                    return Err((
                        400,
                        "Keyword exceeds 2000 character limit",
                        "invalid_keyword",
                    ));
                }
                sanitized
            }
        },
    };

    let location = match body.get("location") {
        None => return Err((400, "Missing required field: location", "missing_location")),
        Some(v) => {
            if let Some(n) = v.as_i64() {
                if n <= 0 {
                    return Err((
                        400,
                        "Location code must be a positive integer",
                        "invalid_location",
                    ));
                }
                Location::Code(n)
            } else if let Some(s) = v.as_str() {
                if s.is_empty() {
                    return Err((400, "Location name must be non-empty", "invalid_location"));
                }
                Location::Name(s.to_string())
            } else {
                return Err((
                    400,
                    "Location must be a positive integer or non-empty string",
                    "invalid_location",
                ));
            }
        }
    };

    let language = match body.get("language") {
        None => "en".to_string(),
        Some(v) => match v.as_str() {
            None => return Err((400, "Field 'language' must be a string", "invalid_language")),
            Some(s) => {
                if s.len() < 2 || s.len() > 5 || !s.chars().all(|c| c.is_ascii_lowercase()) {
                    return Err((
                        400,
                        "Language must be 2-5 lowercase letters (e.g., 'en', 'fr')",
                        "invalid_language",
                    ));
                }
                s.to_string()
            }
        },
    };

    let force_web_search = match body.get("force_web_search") {
        None => false,
        Some(v) => match v.as_bool() {
            None => {
                return Err((
                    400,
                    "Field 'force_web_search' must be a boolean",
                    "invalid_force_web_search",
                ))
            }
            Some(b) => {
                if b && !provider.supports_force_web_search() {
                    return Err((
                        400,
                        "force_web_search is not supported for this provider",
                        "invalid_field_for_provider",
                    ));
                }
                b
            }
        },
    };

    let tag = match body.get("tag") {
        None => None,
        Some(v) => match v.as_str() {
            None => return Err((400, "Field 'tag' must be a string", "invalid_tag")),
            Some(s) => {
                if s.len() > 255 {
                    return Err((400, "Tag exceeds 255 character limit", "invalid_tag"));
                }
                Some(s.to_string())
            }
        },
    };

    Ok(LlmRequest {
        provider,
        keyword,
        location,
        language,
        force_web_search,
        tag,
    })
}

pub fn validate_auth(url: &Url, env: &Env, request_id: &str) -> Result<(), Response> {
    let csvkey = url
        .query_pairs()
        .find(|(k, _)| k == "csvkey")
        .map(|(_, v)| v.to_string())
        .ok_or_else(|| {
            json_error(
                400,
                "Missing required query parameter: csvkey",
                "missing_csvkey",
                request_id,
            )
        })?;

    let expected = read_secret(env, "CSVKEY", request_id)?;

    if !constant_time_eq(csvkey.as_bytes(), expected.as_bytes()) {
        return Err(json_error(
            401,
            "Invalid authentication credentials",
            "unauthorized",
            request_id,
        ));
    }

    Ok(())
}

pub async fn parse_and_validate_body(
    req: &mut Request,
    request_id: &str,
) -> Result<LlmRequest, Response> {
    let text = req.text().await.map_err(|_| {
        json_error(
            400,
            "Failed to read request body",
            "missing_body",
            request_id,
        )
    })?;

    if text.is_empty() {
        return Err(json_error(
            400,
            "Request body is empty",
            "missing_body",
            request_id,
        ));
    }

    let body: Value = serde_json::from_str(&text).map_err(|_| {
        json_error(
            400,
            "Invalid JSON in request body",
            "invalid_json",
            request_id,
        )
    })?;

    validate_fields(&body).map_err(|(status, msg, code)| json_error(status, msg, code, request_id))
}

pub fn read_secret(env: &Env, name: &str, request_id: &str) -> Result<String, Response> {
    env.secret(name).map(|s| s.to_string()).map_err(|_| {
        json_error(
            500,
            &format!("Server configuration error: missing {}", name),
            "missing_config",
            request_id,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- sanitize_keyword ---

    #[test]
    fn sanitize_normal_input_unchanged() {
        assert_eq!(sanitize_keyword("hello world"), "hello world");
    }

    #[test]
    fn sanitize_strips_control_chars() {
        assert_eq!(sanitize_keyword("hello\x00world"), "helloworld");
        assert_eq!(sanitize_keyword("test\x01\x02\x03value"), "testvalue");
    }

    #[test]
    fn sanitize_preserves_space() {
        assert_eq!(sanitize_keyword("hello world"), "hello world");
    }

    #[test]
    fn sanitize_collapses_multiple_spaces() {
        assert_eq!(sanitize_keyword("hello    world"), "hello world");
    }

    #[test]
    fn sanitize_collapses_tabs_and_newlines() {
        assert_eq!(sanitize_keyword("hello\t\n\r  world"), "hello world");
    }

    #[test]
    fn sanitize_trims_leading_trailing() {
        assert_eq!(sanitize_keyword("  hello world  "), "hello world");
    }

    #[test]
    fn sanitize_all_whitespace_becomes_empty() {
        assert_eq!(sanitize_keyword("   \t\n  "), "");
    }

    #[test]
    fn sanitize_all_control_chars_becomes_empty() {
        assert_eq!(sanitize_keyword("\x00\x01\x02"), "");
    }

    #[test]
    fn sanitize_preserves_unicode() {
        assert_eq!(sanitize_keyword("café résumé"), "café résumé");
    }

    #[test]
    fn sanitize_mixed_control_and_whitespace() {
        assert_eq!(
            sanitize_keyword("\x00 hello \x01 \t world \x02"),
            "hello world"
        );
    }

    // --- validate_fields ---

    #[test]
    fn validate_missing_provider() {
        let body = json!({"keyword": "test", "location": 2840});
        let err = validate_fields(&body).unwrap_err();
        assert_eq!(err.2, "missing_provider");
    }

    #[test]
    fn validate_invalid_provider() {
        let body = json!({"provider": "perplexity", "keyword": "test", "location": 2840});
        let err = validate_fields(&body).unwrap_err();
        assert_eq!(err.2, "invalid_provider");
    }

    #[test]
    fn validate_provider_not_string() {
        let body = json!({"provider": 123, "keyword": "test", "location": 2840});
        let err = validate_fields(&body).unwrap_err();
        assert_eq!(err.2, "invalid_provider");
    }

    #[test]
    fn validate_missing_keyword() {
        let body = json!({"provider": "gemini", "location": 2840});
        let err = validate_fields(&body).unwrap_err();
        assert_eq!(err.2, "missing_keyword");
    }

    #[test]
    fn validate_empty_keyword_after_sanitization() {
        let body = json!({"provider": "gemini", "keyword": "   ", "location": 2840});
        let err = validate_fields(&body).unwrap_err();
        assert_eq!(err.2, "invalid_keyword");
    }

    #[test]
    fn validate_keyword_too_long() {
        let long_kw = "a".repeat(2001);
        let body = json!({"provider": "gemini", "keyword": long_kw, "location": 2840});
        let err = validate_fields(&body).unwrap_err();
        assert_eq!(err.2, "invalid_keyword");
    }

    #[test]
    fn validate_keyword_at_max_length() {
        let kw = "a".repeat(2000);
        let body = json!({"provider": "gemini", "keyword": kw, "location": 2840});
        assert!(validate_fields(&body).is_ok());
    }

    #[test]
    fn validate_missing_location() {
        let body = json!({"provider": "gemini", "keyword": "test"});
        let err = validate_fields(&body).unwrap_err();
        assert_eq!(err.2, "missing_location");
    }

    #[test]
    fn validate_location_positive_int() {
        let body = json!({"provider": "gemini", "keyword": "test", "location": 2840});
        let req = validate_fields(&body).unwrap();
        assert!(matches!(req.location, Location::Code(2840)));
    }

    #[test]
    fn validate_location_string() {
        let body = json!({"provider": "gemini", "keyword": "test", "location": "United States"});
        let req = validate_fields(&body).unwrap();
        assert!(matches!(req.location, Location::Name(ref s) if s == "United States"));
    }

    #[test]
    fn validate_location_negative_int() {
        let body = json!({"provider": "gemini", "keyword": "test", "location": -5});
        let err = validate_fields(&body).unwrap_err();
        assert_eq!(err.2, "invalid_location");
    }

    #[test]
    fn validate_location_zero() {
        let body = json!({"provider": "gemini", "keyword": "test", "location": 0});
        let err = validate_fields(&body).unwrap_err();
        assert_eq!(err.2, "invalid_location");
    }

    #[test]
    fn validate_location_empty_string() {
        let body = json!({"provider": "gemini", "keyword": "test", "location": ""});
        let err = validate_fields(&body).unwrap_err();
        assert_eq!(err.2, "invalid_location");
    }

    #[test]
    fn validate_language_default() {
        let body = json!({"provider": "gemini", "keyword": "test", "location": 2840});
        let req = validate_fields(&body).unwrap();
        assert_eq!(req.language, "en");
    }

    #[test]
    fn validate_language_valid() {
        let body =
            json!({"provider": "gemini", "keyword": "test", "location": 2840, "language": "fr"});
        let req = validate_fields(&body).unwrap();
        assert_eq!(req.language, "fr");
    }

    #[test]
    fn validate_language_too_long() {
        let body = json!({"provider": "gemini", "keyword": "test", "location": 2840, "language": "English"});
        let err = validate_fields(&body).unwrap_err();
        assert_eq!(err.2, "invalid_language");
    }

    #[test]
    fn validate_language_has_numbers() {
        let body =
            json!({"provider": "gemini", "keyword": "test", "location": 2840, "language": "e1"});
        let err = validate_fields(&body).unwrap_err();
        assert_eq!(err.2, "invalid_language");
    }

    #[test]
    fn validate_force_web_search_chatgpt_true() {
        let body = json!({"provider": "chatgpt", "keyword": "test", "location": 2840, "force_web_search": true});
        let req = validate_fields(&body).unwrap();
        assert!(req.force_web_search);
    }

    #[test]
    fn validate_force_web_search_chatgpt_false() {
        let body = json!({"provider": "chatgpt", "keyword": "test", "location": 2840, "force_web_search": false});
        let req = validate_fields(&body).unwrap();
        assert!(!req.force_web_search);
    }

    #[test]
    fn validate_force_web_search_gemini_true_error() {
        let body = json!({"provider": "gemini", "keyword": "test", "location": 2840, "force_web_search": true});
        let err = validate_fields(&body).unwrap_err();
        assert_eq!(err.2, "invalid_field_for_provider");
    }

    #[test]
    fn validate_force_web_search_gemini_false_ok() {
        let body = json!({"provider": "gemini", "keyword": "test", "location": 2840, "force_web_search": false});
        assert!(validate_fields(&body).is_ok());
    }

    #[test]
    fn validate_force_web_search_both_true_ok() {
        let body = json!({"provider": "both", "keyword": "test", "location": 2840, "force_web_search": true});
        assert!(validate_fields(&body).is_ok());
    }

    #[test]
    fn validate_force_web_search_not_boolean() {
        let body = json!({"provider": "chatgpt", "keyword": "test", "location": 2840, "force_web_search": "true"});
        let err = validate_fields(&body).unwrap_err();
        assert_eq!(err.2, "invalid_force_web_search");
    }

    #[test]
    fn validate_tag_valid() {
        let body =
            json!({"provider": "gemini", "keyword": "test", "location": 2840, "tag": "my-tag-123"});
        let req = validate_fields(&body).unwrap();
        assert_eq!(req.tag, Some("my-tag-123".to_string()));
    }

    #[test]
    fn validate_tag_too_long() {
        let long_tag = "x".repeat(256);
        let body =
            json!({"provider": "gemini", "keyword": "test", "location": 2840, "tag": long_tag});
        let err = validate_fields(&body).unwrap_err();
        assert_eq!(err.2, "invalid_tag");
    }

    // --- constant_time_eq ---

    #[test]
    fn constant_time_eq_same() {
        assert!(constant_time_eq(b"secret", b"secret"));
    }

    #[test]
    fn constant_time_eq_different_content() {
        assert!(!constant_time_eq(b"secret", b"secre!"));
    }

    #[test]
    fn constant_time_eq_different_length() {
        assert!(!constant_time_eq(b"secret", b"sec"));
    }

    #[test]
    fn constant_time_eq_empty_both() {
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn constant_time_eq_one_empty() {
        assert!(!constant_time_eq(b"secret", b""));
    }
}
