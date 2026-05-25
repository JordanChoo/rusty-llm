# rusty-llm: Product Requirements Document

## 1. Overview

**rusty-llm** is a Rust-based Cloudflare Worker that acts as a stateless HTTP proxy to DataForSEO's AI Optimization LLM Scraper endpoints. It provides a unified, authenticated endpoint for Graph Agents (LangGraph, LangChain, Deep Agents) to retrieve live LLM responses from **Google Gemini**, **OpenAI ChatGPT**, or **both simultaneously** via DataForSEO's scraping infrastructure.

### 1.1 Problems Solved

| Problem | Solution |
|---------|----------|
| **Credential sprawl** | Single set of DataForSEO credentials managed as CF Worker secrets |
| **Inconsistent error handling** | Normalized JSON error responses across all failure modes |
| **Duplicated integration code** | One HTTP endpoint replaces per-agent SDK logic |
| **Multi-LLM complexity** | Single worker supports Gemini, ChatGPT, or both via a `provider` field |
| **Cross-LLM comparison** | Dual-provider mode queries both LLMs in parallel with one request |
| **Opaque failures** | Request IDs, structured logging, and metadata headers enable end-to-end tracing |

### 1.2 Reference Architecture

This worker follows the **same core architecture** as [rusty-serp](https://github.com/jordanChoo/rusty-serp) with targeted enhancements:

- Pure Rust compiled to WASM (no TypeScript wrapper)
- `cdylib` crate type for Cloudflare Workers
- Error-as-value pattern (`Result<T, Response>`)
- Fail-early sequential pipeline
- Pass-through of DataForSEO responses (single-provider mode)
- Constant-time authentication via `csvkey` query parameter
- Enhanced module structure: `lib.rs`, `validation.rs`, `dataforseo.rs`, `errors.rs`, `providers.rs`

---

## 2. Architecture

### 2.1 Module Structure

```
rusty-llm/
  .gitignore
  Cargo.toml
  Cargo.lock
  wrangler.toml
  LICENSE
  README.md
  prd/
    rusty-llm-prd.md
  src/
    lib.rs              # Entry point, routing, orchestration, request ID generation
    validation.rs       # csvkey auth, body parsing, field validation, input sanitization
    dataforseo.rs       # DataForSEO LLM API client (single + dual-provider)
    errors.rs           # JSON error response builders, UTF-8 truncation
    providers.rs        # Provider enum, URL constants, per-provider validation rules
```

### 2.2 Module Dependency Graph

```
lib.rs --> validation.rs --> errors.rs
  |              |
  |              +---------> providers.rs
  |                          ^
  +-----> dataforseo.rs -----+
                |
                +----------> errors.rs
```

- `errors.rs` has zero internal dependencies
- `providers.rs` has zero internal dependencies (defines Provider enum + constants)
- `validation.rs` depends on `errors.rs` and `providers.rs`
- `dataforseo.rs` depends on `errors.rs` and `providers.rs`
- `lib.rs` orchestrates all four

### 2.3 Request Flow (Single Provider)

```
Agent (HTTP POST)
  |
  v
[lib.rs] Generate request_id (UUID v4)
  |
  v
[lib.rs] Route: POST /v1/llm
  |
  v
[validation.rs] validate_auth(csvkey)
  |
  v
[validation.rs] parse_and_validate_body() -> LlmRequest
  |
  v
[providers.rs] validate provider-specific fields
  |
  v
[lib.rs] read_secret(DATAFORSEO_LOGIN)
  |
  v
[lib.rs] read_secret(DATAFORSEO_PASSWORD)
  |
  v
[dataforseo.rs] fetch_llm(request, login, password)
  |
  v
[lib.rs] Attach metadata headers to response
  |
  v
Return DataForSEO response with metadata headers (200)
  OR error response (4xx/5xx) with request_id
```

### 2.4 Request Flow (Dual Provider: `"both"`)

```
Agent (HTTP POST)
  |
  v
[lib.rs] Generate request_id, validate, read secrets
  |
  v
[dataforseo.rs] fetch_both(request, login, password)
  |
  +---> fetch Gemini  ----+
  |                        |  (concurrent via futures::join!)
  +---> fetch ChatGPT ---+
  |
  v
[dataforseo.rs] Assemble envelope response
  |
  v
Return combined envelope with metadata headers (200)
  OR error response if BOTH fail (502)
  OR partial response if ONE fails (207)
```

---

## 3. API Specification

### 3.1 Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/v1/health` | Health check with version and available providers |
| `HEAD` | `/v1/health` | Health probe (headers only, 200) |
| `POST` | `/v1/llm` | Fetch live LLM response from Gemini, ChatGPT, or both |

All other method/path combinations return `404 not_found` or `405 method_not_allowed`.

### 3.2 Health Check Response

```json
{
  "status": "ok",
  "version": "0.1.0",
  "providers": ["gemini", "chatgpt", "both"]
}
```

### 3.3 Authentication

All requests to `/v1/llm` must include a `csvkey` query parameter:

```
POST /v1/llm?csvkey=<your-key>
```

Authentication uses constant-time XOR fold comparison (same as rusty-serp). Leaks key length but prevents timing attacks on content.

### 3.4 Request Body (`POST /v1/llm`)

Content-Type is not validated (intentional, same as rusty-serp).

```json
{
  "provider": "gemini",
  "keyword": "best project management tools for remote teams",
  "location": 2840,
  "language": "en",
  "force_web_search": true,
  "tag": "agent-task-123"
}
```

### 3.5 Request Fields

| Field | Type | Required | Default | Constraints |
|-------|------|----------|---------|-------------|
| `provider` | string | **Yes** | — | `"gemini"`, `"chatgpt"`, or `"both"` (case-insensitive) |
| `keyword` | string | **Yes** | — | Non-empty, max 2000 characters (after sanitization) |
| `location` | integer or string | **Yes** | — | Integer (positive) -> `location_code`, String (non-empty) -> `location_name` |
| `language` | string | No | `"en"` | 2-5 lowercase alpha characters; sent as `language_code` to DataForSEO |
| `force_web_search` | boolean | No | `false` | **ChatGPT only**. Must be boolean if present. Returns 400 if used with `provider: "gemini"` |
| `tag` | string | No | — | Max 255 characters. Passed through to DataForSEO. If omitted, the generated `request_id` is used as the tag |

### 3.6 Input Sanitization

Applied to `keyword` before validation and dispatch:

1. Strip ASCII control characters (0x00-0x1F except 0x20 space)
2. Collapse consecutive whitespace (spaces, tabs, newlines) into a single space
3. Trim leading/trailing whitespace
4. Validate length AFTER sanitization

### 3.7 Response (Single Provider)

On success (HTTP 200): DataForSEO's response body is returned **verbatim** with `Content-Type: application/json` plus metadata headers.

**Metadata headers (always present on 200):**

| Header | Description | Example |
|--------|-------------|---------|
| `X-RustyLLM-Request-Id` | Unique request UUID | `a1b2c3d4-e5f6-7890-abcd-ef1234567890` |
| `X-RustyLLM-Provider` | Provider used | `gemini` |
| `X-RustyLLM-Model` | LLM model version (extracted from response) | `gemini-2.0-flash` |
| `X-RustyLLM-Duration-Ms` | Total upstream request time | `4200` |
| `X-RustyLLM-DFS-Cost-Cents` | DataForSEO cost in USD cents | `40` |
| `X-RustyLLM-DFS-Status` | DataForSEO task status code | `20000` |

To extract model and cost, the worker performs a shallow parse of the response (reads `tasks[0].result[0].model` and `tasks[0].cost`). The body is still returned verbatim — the parse is read-only for header enrichment.

### 3.8 Response (Dual Provider: `"both"`)

When `provider: "both"`, the response is an envelope (HTTP 200 or 207):

```json
{
  "request_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "provider": "both",
  "duration_ms": 8700,
  "gemini": {
    "status": "ok",
    "duration_ms": 4200,
    "model": "gemini-2.0-flash",
    "dfs_cost_cents": 40,
    "response": { /* full DataForSEO Gemini response, verbatim */ }
  },
  "chatgpt": {
    "status": "ok",
    "duration_ms": 8700,
    "model": "gpt-4o",
    "dfs_cost_cents": 40,
    "response": { /* full DataForSEO ChatGPT response, verbatim */ }
  }
}
```

**Partial failure (HTTP 207 Multi-Status):**

If one provider succeeds and the other fails, the worker returns 207 with the successful result and an error object for the failed one:

```json
{
  "request_id": "...",
  "provider": "both",
  "duration_ms": 12100,
  "gemini": {
    "status": "ok",
    "duration_ms": 4200,
    "model": "gemini-2.0-flash",
    "dfs_cost_cents": 40,
    "response": { /* ... */ }
  },
  "chatgpt": {
    "status": "error",
    "duration_ms": 12100,
    "error": "DataForSEO returned non-success status",
    "code": "dataforseo_error",
    "dataforseo_status": 500,
    "dataforseo_body": "..."
  }
}
```

If **both** providers fail, return HTTP 502 with both error objects.

### 3.9 Error Responses

All error responses include the `request_id`:

```json
{
  "request_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "error": "Human-readable message",
  "code": "machine_readable_code"
}
```

The `X-RustyLLM-Request-Id` header is also present on all error responses.

---

## 4. DataForSEO Integration

### 4.1 Upstream Endpoints

| Provider | DataForSEO Endpoint |
|----------|-------------------|
| Gemini | `https://api.dataforseo.com/v3/ai_optimization/gemini/llm_scraper/live/advanced` |
| ChatGPT | `https://api.dataforseo.com/v3/ai_optimization/chat_gpt/llm_scraper/live/advanced` |

### 4.2 Request Construction

The worker builds a DataForSEO task object:

```json
[
  {
    "keyword": "<sanitized keyword>",
    "location_code": 2840,
    "language_code": "en",
    "force_web_search": true,
    "tag": "<user tag or request_id>"
  }
]
```

Key behaviors:
- Request body is always a JSON array containing exactly one task object
- `location` field is mapped to `location_code` (integer) or `location_name` (string) based on input type
- `language` is always sent as `language_code`
- `force_web_search` is only included in the payload when `provider == "chatgpt"` AND the value is `true`
- `tag` is set to the caller's tag if provided, otherwise the generated `request_id` (enables correlation in DataForSEO dashboard)

### 4.3 Authentication to DataForSEO

HTTP Basic Auth: `Authorization: Basic base64(login:password)`

Credentials read from CF Worker secrets: `DATAFORSEO_LOGIN`, `DATAFORSEO_PASSWORD`.

### 4.4 Fetch Timeout

An explicit 120-second timeout is set on the upstream fetch using `AbortSignal::timeout(120_000)`:

```rust
let abort = AbortSignal::timeout(120_000);
let mut init = RequestInit::new();
init.with_signal(&abort);
```

This provides deterministic timeout behavior rather than relying on error message string matching.

### 4.5 Retry Strategy

Before returning a 502 to the caller, the worker performs **one retry** on transient failures:

| DataForSEO Status | Action |
|-------------------|--------|
| 429 (Rate Limited) | Wait 1 second, retry once |
| 503 (Service Unavailable) | Wait 1 second, retry once |
| All other non-2xx | Fail immediately (no retry) |
| Timeout | Fail immediately (no retry) |

The retry uses `worker::delay(Duration::from_secs(1))`. If the retry also fails, the error from the **second** attempt is returned to the caller.

In dual-provider mode, each provider's fetch has its own independent retry logic.

### 4.6 Response Handling

| Condition | Worker Response |
|-----------|----------------|
| Fetch timeout (AbortSignal fires) | 504 `dataforseo_timeout` |
| Fetch network error | 502 `dataforseo_error` with `dataforseo_status: 0` |
| Non-2xx after retry | 502 with upstream status + truncated body (4KB max) |
| 2xx from DataForSEO | 200, body passed through with metadata headers |

### 4.7 Metadata Extraction (Shallow Parse)

On a successful 2xx response, the worker performs a read-only shallow parse to extract metadata for response headers:

```rust
// Parse only what's needed for headers
let parsed: serde_json::Value = serde_json::from_str(&body)?;
let model = parsed["tasks"][0]["result"][0]["model"].as_str();
let cost = parsed["tasks"][0]["cost"].as_f64();
let dfs_status = parsed["tasks"][0]["status_code"].as_i64();
```

If parsing fails (malformed response), the metadata headers are omitted but the response is still returned verbatim. The pass-through guarantee is never violated — metadata extraction is best-effort.

### 4.8 DataForSEO Response Structure (Consumer Reference)

Both endpoints return similar top-level structures. The worker does NOT transform these. This section is for consumer reference.

#### Gemini Response Items

| Item Type | Description |
|-----------|-------------|
| `gemini_text` | Text content block with markdown, original text, sources |
| `gemini_table` | Table with structured header/content arrays |
| `gemini_images` | Image group with individual image elements |

#### ChatGPT Response Items

| Item Type | Description |
|-----------|-------------|
| `chat_gpt_text` | Text content block with markdown, sources, brand entities |
| `chat_gpt_table` | Table with structured header/content arrays |
| `chat_gpt_navigation_list` | Navigation list with title and sources |
| `chat_gpt_images` | Image group with individual image elements |
| `chat_gpt_local_businesses` | Local business listings with ratings |
| `chat_gpt_products` | Product listings with prices and merchant data |

#### Additional ChatGPT-specific fields

- `check_url` — Direct URL to the ChatGPT conversation
- `search_results` — Web search outputs retrieved by the model
- `fan_out_queries` — Related/derived search queries
- `brand_entities` — Brands mentioned in the response

---

## 5. Provider Module (`src/providers.rs`)

### 5.1 Purpose

Centralizes all provider-specific logic in one location. Adding a new provider (e.g., Perplexity) should only require changes to this file and `dataforseo.rs`.

### 5.2 Types

```rust
pub enum Provider {
    Gemini,
    ChatGpt,
    Both,
}

impl Provider {
    pub fn from_str(s: &str) -> Option<Self> { /* case-insensitive */ }
    pub fn url(&self) -> &'static str { /* DataForSEO endpoint URL */ }
    pub fn supports_force_web_search(&self) -> bool { /* true only for ChatGpt */ }
    pub fn name(&self) -> &'static str { /* lowercase: "gemini", "chatgpt", "both" */ }
}
```

### 5.3 URL Constants

```rust
pub const GEMINI_URL: &str = "https://api.dataforseo.com/v3/ai_optimization/gemini/llm_scraper/live/advanced";
pub const CHATGPT_URL: &str = "https://api.dataforseo.com/v3/ai_optimization/chat_gpt/llm_scraper/live/advanced";
```

### 5.4 Provider-Specific Validation

Validation rules that differ by provider:

| Rule | Gemini | ChatGPT | Both |
|------|--------|---------|------|
| `force_web_search` allowed | No (400 error if present and `true`) | Yes | Yes (applied to ChatGPT leg only) |

---

## 6. Configuration

### 6.1 Cargo.toml

```toml
[package]
name = "rusty-llm"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
worker = "0.8"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
base64 = "0.22"
console_error_panic_hook = "0.1"
getrandom = { version = "0.2", features = ["js"] }

[profile.release]
opt-level = "z"
lto = true
strip = true
```

Note: `getrandom` with the `js` feature provides cryptographically random bytes in WASM for UUID v4 generation.

### 6.2 Wrangler Configuration

```toml
name = "rusty-llm"
main = "build/worker/shim.mjs"
compatibility_date = "2026-05-03"

[build]
command = "curl https://sh.rustup.rs -sSf | sh -s -- -y --default-toolchain stable --profile minimal && . \"$HOME/.cargo/env\" && rustup target add wasm32-unknown-unknown && cargo install worker-build && worker-build --release"

[env.staging]
name = "rusty-llm-staging"

[env.production]
name = "rusty-llm"
```

### 6.3 Secrets

| Secret | Purpose |
|--------|---------|
| `CSVKEY` | Authenticates inbound agent requests |
| `DATAFORSEO_LOGIN` | DataForSEO API email/login |
| `DATAFORSEO_PASSWORD` | DataForSEO API key |

Set via `wrangler secret put <NAME>` (per environment).

Local development uses `.dev.vars` (gitignored).

### 6.4 Deployment

| Command | Worker Name | Purpose |
|---------|-------------|---------|
| `wrangler deploy` | `rusty-llm` | Dev |
| `wrangler deploy --env staging` | `rusty-llm-staging` | Staging |
| `wrangler deploy --env production` | `rusty-llm` | Production |

---

## 7. Error Taxonomy

### 7.1 Client Errors (4xx)

| Status | Code | Trigger |
|--------|------|---------|
| 400 | `missing_csvkey` | No `csvkey` query parameter |
| 400 | `missing_body` | Empty request body |
| 400 | `invalid_json` | Body is not valid JSON |
| 400 | `missing_provider` | No `provider` field |
| 400 | `invalid_provider` | `provider` not "gemini", "chatgpt", or "both" |
| 400 | `missing_keyword` | No `keyword` field |
| 400 | `invalid_keyword` | Empty string (after sanitization) or exceeds 2000 chars |
| 400 | `missing_location` | No `location` field |
| 400 | `invalid_location` | Not a positive integer or non-empty string |
| 400 | `invalid_language` | Not 2-5 lowercase alpha characters (when provided) |
| 400 | `invalid_force_web_search` | Not a boolean (when provided) |
| 400 | `invalid_field_for_provider` | `force_web_search: true` used with `provider: "gemini"` |
| 400 | `invalid_tag` | Exceeds 255 characters (when provided) |
| 401 | `unauthorized` | csvkey mismatch |
| 404 | `not_found` | Unknown path |
| 405 | `method_not_allowed` | Wrong HTTP method for known path |

### 7.2 Server Errors (5xx)

| Status | Code | Trigger |
|--------|------|---------|
| 500 | `missing_config` | Required secret not configured in CF |
| 502 | `dataforseo_error` | Upstream non-2xx after retry (includes `dataforseo_status` + truncated body) |
| 504 | `dataforseo_timeout` | Explicit 120s AbortSignal fired |

### 7.3 Multi-Status (Dual Provider)

| Status | Trigger |
|--------|---------|
| 200 | Both providers succeeded |
| 207 | One provider succeeded, one failed |
| 502 | Both providers failed |

### 7.4 Error Response Format

All errors include `request_id`:

```json
{
  "request_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "error": "Human-readable message",
  "code": "machine_readable_code"
}
```

**DataForSEO upstream errors (502):**
```json
{
  "request_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "error": "DataForSEO returned non-success status",
  "code": "dataforseo_error",
  "dataforseo_status": 403,
  "dataforseo_body": "<truncated to 4KB, UTF-8 safe>"
}
```

---

## 8. Observability

### 8.1 Request ID

Every inbound request generates a UUID v4 via `getrandom`. This ID:
- Appears in the `X-RustyLLM-Request-Id` response header (all responses, including errors)
- Is included in the JSON body of error responses
- Is sent as the DataForSEO `tag` if the caller didn't provide one
- Is included in structured log output

### 8.2 Structured Logging

Every request produces one structured JSON log line via `console_log!`:

```json
{
  "req_id": "a1b2c3d4-...",
  "provider": "gemini",
  "keyword_len": 42,
  "location": "2840",
  "duration_ms": 4200,
  "status": 200,
  "dfs_status": 20000,
  "retried": false
}
```

Accessible via `wrangler tail` in real-time or Cloudflare dashboard logs.

### 8.3 Metadata Headers

Present on all successful (200/207) responses:

| Header | Description |
|--------|-------------|
| `X-RustyLLM-Request-Id` | Unique request UUID |
| `X-RustyLLM-Provider` | Provider(s) used |
| `X-RustyLLM-Model` | Model version (single provider only) |
| `X-RustyLLM-Duration-Ms` | Total upstream time |
| `X-RustyLLM-DFS-Cost-Cents` | DataForSEO cost in USD cents |
| `X-RustyLLM-DFS-Status` | DataForSEO task status code |
| `X-RustyLLM-Retried` | `true` if a retry was attempted |

---

## 9. Design Principles

Core principles carried forward from rusty-serp:

1. **Error-as-Value** — Every function returns `Result<T, Response>` where `Err` IS the HTTP response. No custom error type hierarchies.
2. **Fail-Early Chain** — Sequential pipeline, short-circuits on first failure via `?` operator or `.map_err()`.
3. **Pass-Through** — DataForSEO responses returned verbatim in single-provider mode. Metadata extracted read-only for headers.
4. **Constant-Time Auth** — XOR fold comparison prevents timing attacks.
5. **Pure Rust/WASM** — No TypeScript shell, no Node.js runtime, single `.wasm` binary.
6. **Minimal Dependencies** — `worker`, `serde`, `serde_json`, `base64`, `console_error_panic_hook`, `getrandom`.
7. **Size Optimized** — `opt-level = "z"`, LTO, strip symbols. Target binary under 500KB.

New principles for rusty-llm:

8. **Observable by Default** — Every request gets a UUID, structured log line, and metadata headers. Zero-config debugging.
9. **Fail Loudly** — Invalid field combinations return errors rather than being silently ignored. Agents should never get unexpected behavior.
10. **Graceful Degradation** — Dual-provider mode returns partial results (207) rather than failing entirely when one provider is down.
11. **Single Retry** — Transient failures (429/503) get one retry attempt before surfacing to the caller.

---

## 10. Differences from rusty-serp

| Aspect | rusty-serp | rusty-llm |
|--------|-----------|-----------|
| Primary endpoint | `POST /v1/serp` | `POST /v1/llm` |
| Upstream API | SERP Google Organic | AI Optimization LLM Scraper |
| Provider selection | N/A (Google only) | `provider`: `"gemini"`, `"chatgpt"`, or `"both"` |
| Dual-provider mode | No | Yes (concurrent fetch, combined envelope) |
| `depth` field | Yes (1-700, default 10) | No (not applicable) |
| `device` field | Yes ("desktop"/"mobile") | No (not applicable) |
| `ai_optimized` field | Yes (selects `.ai` endpoint) | No (replaced by `provider`) |
| `force_web_search` field | No | Yes (ChatGPT only, strict validation) |
| `tag` field | No | Yes (optional, defaults to request_id) |
| `keyword` max length | 700 chars | 2000 chars |
| Input sanitization | No | Yes (control chars, whitespace collapse) |
| Request ID | No | Yes (UUID v4 on every request) |
| Metadata headers | No | Yes (7 headers on all 200 responses) |
| Structured logging | No | Yes (JSON log per request) |
| Retry on transient | No | Yes (single retry on 429/503) |
| Explicit timeout | String-matching heuristic | AbortSignal (120s deterministic) |
| Module count | 4 | 5 (adds `providers.rs`) |
| Additional dependency | — | `getrandom` (UUID generation) |
| Health check | `{"status": "ok"}` | `{"status":"ok","version":"0.1.0","providers":[...]}` |

---

## 11. Example Usage

### 11.1 Gemini Request

```bash
curl -X POST "https://rusty-llm.<subdomain>.workers.dev/v1/llm?csvkey=your-secret-key" \
  -H "Content-Type: application/json" \
  -d '{
    "provider": "gemini",
    "keyword": "best practices for microservices architecture",
    "location": 2840,
    "language": "en"
  }'
```

Response headers:
```
X-RustyLLM-Request-Id: a1b2c3d4-e5f6-7890-abcd-ef1234567890
X-RustyLLM-Provider: gemini
X-RustyLLM-Model: gemini-2.0-flash
X-RustyLLM-Duration-Ms: 4200
X-RustyLLM-DFS-Cost-Cents: 40
X-RustyLLM-DFS-Status: 20000
X-RustyLLM-Retried: false
```

### 11.2 ChatGPT Request (with web search)

```bash
curl -X POST "https://rusty-llm.<subdomain>.workers.dev/v1/llm?csvkey=your-secret-key" \
  -H "Content-Type: application/json" \
  -d '{
    "provider": "chatgpt",
    "keyword": "top AI coding assistants 2026",
    "location": "United States",
    "language": "en",
    "force_web_search": true,
    "tag": "competitive-analysis-001"
  }'
```

### 11.3 Dual-Provider Request (Both)

```bash
curl -X POST "https://rusty-llm.<subdomain>.workers.dev/v1/llm?csvkey=your-secret-key" \
  -H "Content-Type: application/json" \
  -d '{
    "provider": "both",
    "keyword": "best CRM software for startups",
    "location": 2840,
    "language": "en",
    "force_web_search": true
  }'
```

Response (200):
```json
{
  "request_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "provider": "both",
  "duration_ms": 8700,
  "gemini": {
    "status": "ok",
    "duration_ms": 4200,
    "model": "gemini-2.0-flash",
    "dfs_cost_cents": 40,
    "response": { "version": "...", "tasks": [...] }
  },
  "chatgpt": {
    "status": "ok",
    "duration_ms": 8700,
    "model": "gpt-4o",
    "dfs_cost_cents": 40,
    "response": { "version": "...", "tasks": [...] }
  }
}
```

### 11.4 Invalid Field Combination (Error)

```bash
curl -X POST "https://rusty-llm.<subdomain>.workers.dev/v1/llm?csvkey=your-secret-key" \
  -d '{"provider":"gemini","keyword":"test","location":2840,"force_web_search":true}'
```

Response (400):
```json
{
  "request_id": "...",
  "error": "force_web_search is only supported for the chatgpt provider",
  "code": "invalid_field_for_provider"
}
```

### 11.5 Health Check

```bash
curl "https://rusty-llm.<subdomain>.workers.dev/v1/health"
```
```json
{
  "status": "ok",
  "version": "0.1.0",
  "providers": ["gemini", "chatgpt", "both"]
}
```

---

## 12. Agent Integration

### 12.1 LangChain/LangGraph Tool Definition

```python
from langchain_core.tools import tool
import httpx

@tool
def query_llm(
    provider: str,
    keyword: str,
    location: int | str,
    language: str = "en",
    force_web_search: bool = False,
    tag: str | None = None,
) -> dict:
    """Query a live LLM (Gemini, ChatGPT, or both) via DataForSEO for real-time AI responses.

    Use provider="both" to compare how different LLMs respond to the same query.
    The force_web_search flag only applies to ChatGPT.
    """
    payload = {
        "provider": provider,
        "keyword": keyword,
        "location": location,
        "language": language,
    }
    if force_web_search and provider in ("chatgpt", "both"):
        payload["force_web_search"] = True
    if tag:
        payload["tag"] = tag

    resp = httpx.post(
        f"{RUSTY_LLM_URL}/v1/llm?csvkey={CSVKEY}",
        json=payload,
        timeout=130.0,  # DataForSEO allows up to 120s
    )
    resp.raise_for_status()

    # Access metadata without parsing the full response
    request_id = resp.headers.get("X-RustyLLM-Request-Id")
    model = resp.headers.get("X-RustyLLM-Model")

    return resp.json()
```

### 12.2 Deep Agents HTTP Tool

```python
{
    "name": "query_llm",
    "type": "http",
    "config": {
        "method": "POST",
        "url": "${RUSTY_LLM_URL}/v1/llm?csvkey=${CSVKEY}",
        "headers": {"Content-Type": "application/json"},
        "timeout": 130
    }
}
```

### 12.3 Common Agent Patterns

**Brand monitoring (dual-provider):**
```json
{
  "provider": "both",
  "keyword": "best [your-product-category] tools",
  "location": 2840,
  "force_web_search": true,
  "tag": "brand-monitor-daily"
}
```

**Competitive intelligence:**
```json
{
  "provider": "chatgpt",
  "keyword": "[competitor name] alternatives",
  "location": 2840,
  "force_web_search": true,
  "tag": "competitor-sweep"
}
```

---

## 13. Explicitly Out of Scope (v1.0)

- **No caching** (no KV, no Cache API) — LLM responses are inherently dynamic
- **No rate limiting** — relies on DataForSEO's built-in 2000 req/min limit
- **No response transformation** — DataForSEO JSON passed verbatim (metadata extraction is read-only)
- **No batch/queue mode** — Live mode only, one task per request
- **No Content-Type validation** on inbound requests
- **No location_coordinate support** — Only `location_code` and `location_name`
- **No Perplexity provider** — Architecture supports it; planned for v1.1
- **No opt-in envelope mode for single provider** — Headers provide metadata; full envelope reserved for `"both"` mode

---

## 14. Future Enhancements (v1.1+)

| Feature | Description | Effort |
|---------|-------------|--------|
| Perplexity provider | Add `"perplexity"` to provider enum, new URL constant | Low |
| Opt-in envelope mode | `?envelope=true` wraps single-provider responses in metadata envelope | Low |
| Claude provider | If/when DataForSEO adds Claude scraping | Low |
| KV-based response caching | Cache identical keyword+location+provider combos with TTL | Medium |
| Webhook/callback mode | POST results to a callback URL instead of synchronous response | Medium |
| Rate limit tracking | Track usage against 2000/min via CF Durable Objects | Medium |

---

## 15. DataForSEO Rate Limits & Constraints

| Constraint | Value |
|------------|-------|
| Max API calls per minute | 2000 |
| Tasks per call | 1 (Live mode) |
| Max execution time per task | 120 seconds |
| Max keyword length | 2000 characters |
| Max tag length | 255 characters |
| Cost per Gemini task | ~$0.004 USD |
| Cost per ChatGPT task | ~$0.004 USD |
| Cost per "both" request | ~$0.008 USD (two tasks) |

---

## 16. Acceptance Criteria

### Core Functionality

1. `GET /v1/health` returns `{"status":"ok","version":"0.1.0","providers":["gemini","chatgpt","both"]}` with HTTP 200
2. `HEAD /v1/health` returns HTTP 200 with no body
3. `POST /v1/llm` with valid Gemini request returns DataForSEO Gemini response verbatim (200)
4. `POST /v1/llm` with valid ChatGPT request returns DataForSEO ChatGPT response verbatim (200)
5. `POST /v1/llm` with `provider: "both"` returns combined envelope with both responses (200)
6. Dual-provider mode with one failure returns 207 with partial results
7. Dual-provider mode with both failures returns 502

### Provider-Specific Behavior

8. `force_web_search: true` is included in DataForSEO payload only for ChatGPT provider
9. `force_web_search: true` with `provider: "gemini"` returns 400 `invalid_field_for_provider`
10. `force_web_search: true` with `provider: "both"` applies only to the ChatGPT leg

### Authentication & Validation

11. Missing `csvkey` returns 400 `missing_csvkey`
12. Invalid `csvkey` returns 401 `unauthorized`
13. Invalid `provider` returns 400 `invalid_provider`
14. Missing required fields return appropriate 400 error codes
15. Keywords with control characters are sanitized before dispatch
16. `language` field rejects non-alpha or length-violating values

### Reliability

17. DataForSEO timeout (120s) produces 504 `dataforseo_timeout` via AbortSignal
18. DataForSEO 429/503 triggers one retry before returning 502
19. DataForSEO non-2xx (non-retryable) produces 502 with upstream status and truncated body

### Observability

20. Every response includes `X-RustyLLM-Request-Id` header
21. Successful responses include all 7 metadata headers
22. Error responses include `request_id` in the JSON body
23. Every request produces one structured JSON log line

### Operational

24. Binary compiles to under 500KB WASM
25. Deploys successfully to Cloudflare Workers in dev, staging, and production environments
26. All secrets are read from CF secrets (never hardcoded)
27. `tag` defaults to `request_id` when not provided by the caller

---

## 17. Implementation Checklist

### Phase 1: Foundation

- [ ] Initialize Cargo project with `cdylib` crate type
- [ ] Configure `wrangler.toml` with build command, environments
- [ ] Create `.gitignore` (`.env`, `.dev.vars`, `target/`, `build/`, `node_modules/`, `.wrangler/`)
- [ ] Create `.dev.vars.example` with placeholder secret names

### Phase 2: Core Modules

- [ ] Implement `src/errors.rs` — `json_error()`, `dataforseo_error()`, `truncate()`
- [ ] Implement `src/providers.rs` — `Provider` enum, URL constants, `supports_force_web_search()`
- [ ] Implement `src/validation.rs` — `validate_auth()`, `parse_and_validate_body()`, `sanitize_keyword()`, `read_secret()`, `constant_time_eq()`
- [ ] Implement `src/dataforseo.rs` — `fetch_llm()` with provider-based URL selection, retry logic, timeout
- [ ] Implement dual-provider in `src/dataforseo.rs` — `fetch_both()` with `futures::join!`

### Phase 3: Orchestration

- [ ] Implement `src/lib.rs` — routing, UUID generation, `handle_llm()` orchestration
- [ ] Implement metadata header attachment (shallow parse for model/cost/status)
- [ ] Implement structured logging (`console_log!` JSON per request)
- [ ] Implement enhanced health check response

### Phase 4: Validation & Deployment

- [ ] Test single-provider Gemini manually with curl
- [ ] Test single-provider ChatGPT manually with curl
- [ ] Test dual-provider mode manually with curl
- [ ] Test error cases (bad auth, invalid fields, provider mismatch)
- [ ] Test retry behavior (simulate 429 if possible)
- [ ] Verify metadata headers present on all responses
- [ ] Verify request_id in error responses
- [ ] Deploy to staging and verify
- [ ] Deploy to production
- [ ] Write `README.md` with API docs, deployment guide, agent integration examples
