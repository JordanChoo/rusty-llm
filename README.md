# rusty-llm

Rust Cloudflare Worker that proxies DataForSEO's AI Optimization LLM Scraper endpoints (Gemini and ChatGPT). Provides unified request validation, dual-provider concurrent fetching, automatic retry on transient errors, metadata header enrichment, and structured logging.

## Why This Exists

Most agent frameworks want a single tool-like HTTP endpoint, not provider-specific SDK logic scattered across workflows. `rusty-llm` centralizes DataForSEO credentials, normalizes request validation, exposes a stable request/response contract, and makes it easy to compare Gemini and ChatGPT behavior from the same caller.

You can use it for:

- **Agent tool integration**: LangChain, LangGraph, Deep Agents, or custom orchestration layers can call one endpoint instead of managing provider-specific request shaping
- **Cross-model comparison**: A single `"both"` request runs Gemini and ChatGPT concurrently and returns a correlated envelope
- **Live AI search monitoring**: Query DataForSEO's live LLM scraper endpoints without exposing upstream credentials to every client
- **Prompt and query evaluation**: Replay the same keyword, location, and language across providers with a shared request ID and common error model
- **Operational simplicity**: Cloudflare Worker secrets keep the auth surface small and the deployment model lightweight

## Key Features

- **Dual-provider mode**: Query Gemini and ChatGPT concurrently with a single request
- **Response metadata**: Request ID, provider, duration, retry state, and single-provider DFS metadata on successful responses
- **Request IDs**: UUID v4 generated for every `POST /v1/llm` request and propagated through headers, error bodies, logs, and default DFS tags
- **Automatic retry**: Single retry on 429/503 from DataForSEO
- **Input normalization and validation**: Strict field validation with descriptive error codes and keyword sanitization before dispatch
- **Pass-through success responses**: Single-provider DataForSEO bodies are returned unchanged, with headers layered on top
- **Structured logging**: Compact JSON request logs for single-provider success responses and dual-provider aggregate responses via `console_log!`

## Typical Use Cases

- **SEO and brand monitoring agents**: Ask "what does Gemini or ChatGPT say about this keyword right now?" without embedding provider-specific calling logic
- **Research pipelines**: Compare answer shape, citations, or commercial intent across both providers in one call
- **Ops-friendly tool endpoints**: Hand downstream agents one stateless HTTP primitive with predictable auth and validation rules
- **Regression testing**: Re-run the same query set against both providers while preserving correlation IDs and upstream tags
- **Internal dashboards**: Show per-request timing, provider outcome, and retry information using headers plus the JSON envelope

## Architecture

```
Agent Request
     │
     ▼
┌──────────────────┐
│  lib.rs (router) │  UUID generation, routing, orchestration
└────────┬─────────┘
         │
    ┌────┴────┐
    ▼         ▼
validate   read secrets
    │         │
    └────┬────┘
         ▼
┌──────────────────┐
│  dataforseo.rs   │  Single/dual fetch, retry logic
└────────┬─────────┘
         │
    ┌────┴────┐          (concurrent for "both" mode)
    ▼         ▼
 Gemini    ChatGPT
 endpoint  endpoint
```

**Modules:**
- `errors.rs`: JSON error response builders and UTF-8 safe truncation
- `providers.rs`: Provider enum, URL constants, and per-provider feature flags
- `validation.rs`: Auth, body parsing, field validation, and input sanitization
- `dataforseo.rs`: HTTP client, retry logic, and the dual-provider envelope

### Request Lifecycle

1. `lib.rs` generates a UUID v4 request ID for each `POST /v1/llm` request.
2. `validation.rs` authenticates the `csvkey` query parameter with a constant-time byte comparison.
3. The request body is read, parsed as JSON, and validated into an `LlmRequest`.
4. The worker loads `DATAFORSEO_LOGIN` and `DATAFORSEO_PASSWORD` from Cloudflare Worker secrets.
5. `dataforseo.rs` maps the internal request into the exact DataForSEO task payload.
6. The worker dispatches either one provider request or two concurrent requests with `futures::join!`.
7. On single-provider success, the upstream JSON body is returned verbatim and enriched with metadata headers.
8. On dual-provider success or partial failure, the worker assembles a combined envelope with per-provider outcomes.
9. Successful single-provider responses and aggregate dual-provider responses emit a structured JSON log line.

### Repository Layout

```text
rusty-llm/
├── Cargo.toml          # Rust crate metadata and runtime dependencies
├── wrangler.toml       # Worker entrypoint, compatibility date, build command, environments
├── README.md           # Project overview and operator/consumer documentation
├── prd/                # Product and architecture reference docs
├── src/
│   ├── lib.rs          # Router, orchestration, health endpoints, metadata headers, logging
│   ├── validation.rs   # Auth, request parsing, sanitization, field validation
│   ├── providers.rs    # Provider enum, URLs, provider capabilities
│   ├── dataforseo.rs   # Upstream request construction, retry logic, dual-provider envelope
│   └── errors.rs       # Error JSON builders and safe truncation helpers
└── tests/
    └── e2e.sh          # Deployed-worker black-box test harness
```

## Quick Start

### Prerequisites

- Rust (stable) + `wasm32-unknown-unknown` target
- `worker-build` (`cargo install worker-build`)
- Node.js 22+ (for wrangler)
- DataForSEO account with API credentials

### Setup

```bash
git clone <repo-url> && cd rusty-llm
cp .dev.vars.example .dev.vars
# Edit .dev.vars with your credentials

# Build
worker-build --release

# Deploy
npx wrangler deploy

# Set secrets
npx wrangler secret put CSVKEY
npx wrangler secret put DATAFORSEO_LOGIN
npx wrangler secret put DATAFORSEO_PASSWORD
```

## API Reference

### Health Check

```
GET /v1/health
```

**Response (200):**
```json
{
  "status": "ok",
  "version": "0.1.0",
  "providers": ["gemini", "chatgpt", "both"],
  "secrets_configured": true
}
```

`HEAD /v1/health` returns headers only (for uptime probes).

### LLM Scraper

```
POST /v1/llm?csvkey=<your-key>
Content-Type: application/json
```

#### Route Behavior

| Method | Path | Behavior |
|--------|------|----------|
| `GET` | `/v1/health` | Returns health JSON |
| `HEAD` | `/v1/health` | Returns headers only, for uptime probes |
| `POST` | `/v1/llm` | Dispatches to Gemini, ChatGPT, or both |
| any other method | `/v1/health` or `/v1/llm` | Returns `405 method_not_allowed` |
| any method | any other path | Returns `404 not_found` |

#### Request Fields

| Field | Type | Required | Default | Constraints |
|-------|------|----------|---------|-------------|
| `provider` | string | yes | none | `"gemini"`, `"chatgpt"`, or `"both"` (case-insensitive) |
| `keyword` | string | yes | none | 1–2000 chars after sanitization |
| `location` | int or string | yes | none | Positive integer (DFS code) or non-empty string (location name) |
| `language` | string | no | `"en"` | 2–5 lowercase letters |
| `force_web_search` | boolean | no | `false` | Only valid for `chatgpt` and `both` |
| `tag` | string | no | none | Max 255 chars |

#### Validation and Normalization Rules

- `provider` is parsed case-insensitively into the internal `Provider` enum
- `keyword` is sanitized before validation:
  - ASCII control characters are stripped
  - whitespace is collapsed with `split_whitespace()`
  - leading and trailing whitespace are removed as part of the normalization pass
- `keyword` length is enforced **after** sanitization, not before it
- `location` is mapped to `location_code` when numeric and `location_name` when string
- `language` defaults to `"en"` and must be 2 to 5 lowercase ASCII letters
- `force_web_search` must be a boolean and is rejected for Gemini when `true`
- `tag` is optional; when omitted, the generated `request_id` becomes the DFS tag
- Validation errors short-circuit before any upstream network call is attempted

#### Example Request

```bash
curl -X POST "https://<worker-url>/v1/llm?csvkey=<key>" \
  -H "Content-Type: application/json" \
  -d '{
    "provider": "gemini",
    "keyword": "best coffee shops in seattle",
    "location": 2840,
    "language": "en"
  }'
```

#### Upstream Task Mapping

The worker maps each inbound request to a DataForSEO task array containing exactly one task object:

| Inbound field | Upstream field | Notes |
|---------------|----------------|-------|
| `keyword` | `keyword` | Uses the sanitized keyword |
| `location` integer | `location_code` | Positive DFS location code |
| `location` string | `location_name` | Non-empty location name |
| `language` | `language_code` | Defaults to `"en"` |
| `force_web_search` | `force_web_search` | Included only when supported and `true` |
| `tag` | `tag` | Uses caller value or falls back to `request_id` |

#### Single-Provider Response

Returns the DataForSEO response body directly with metadata headers attached.

#### Dual-Provider Response ("both" mode)

Returns an envelope:
```json
{
  "request_id": "a1b2c3d4-...",
  "provider": "both",
  "duration_ms": 4500,
  "gemini": {
    "status": "ok",
    "duration_ms": 3200,
    "retried": false,
    "model": "gemini-2.0-flash",
    "response": { ... }
  },
  "chatgpt": {
    "status": "ok",
    "duration_ms": 4500,
    "retried": false,
    "model": "gpt-4o",
    "response": { ... }
  }
}
```

HTTP status codes for dual mode:
- `200`: Both providers succeeded
- `207`: One provider failed and the other succeeded
- `502`: Both providers failed

#### Dual-Provider Semantics

- Gemini and ChatGPT are dispatched concurrently with `futures::join!`
- Overall duration is measured around the concurrent section, so it tracks the slower provider rather than the sum of both
- Each provider sub-object includes its own status, duration, retry state, and either a `response` object or an error payload
- The top-level HTTP status reflects the aggregate outcome:
  - `200` when both provider requests succeed
  - `207` when exactly one succeeds
  - `502` when both fail
- The top-level `X-RustyLLM-Retried` header is set to `true` if either provider required a retry

### Metadata Headers

#### Single-Provider Success Responses

Present on successful Gemini or ChatGPT passthrough responses:

| Header | Description | Example |
|--------|-------------|---------|
| `X-RustyLLM-Request-Id` | UUID v4 request identifier | `a1b2c3d4-e5f6-...` |
| `X-RustyLLM-Provider` | Provider used | `gemini` |
| `X-RustyLLM-Model` | LLM model from DFS response | `gemini-2.0-flash` |
| `X-RustyLLM-Duration-Ms` | Total processing time | `4200` |
| `X-RustyLLM-DFS-Cost-Cents` | DataForSEO cost in cents | `4` |
| `X-RustyLLM-DFS-Status` | DataForSEO task status code | `20000` |
| `X-RustyLLM-Retried` | Whether a retry was triggered | `false` |

#### Dual-Provider Success And Partial-Success Responses

Present on `"both"` responses:

| Header | Description |
|--------|-------------|
| `X-RustyLLM-Request-Id` | UUID v4 request identifier |
| `X-RustyLLM-Provider` | Always `"both"` |
| `X-RustyLLM-Duration-Ms` | Total wall-clock duration for the concurrent request |
| `X-RustyLLM-Retried` | `true` if either provider retried |

### Error Codes

| Code | HTTP | Meaning |
|------|------|---------|
| `missing_csvkey` | 400 | No `csvkey` query parameter |
| `unauthorized` | 401 | Invalid csvkey |
| `missing_body` | 400 | Empty request body |
| `invalid_json` | 400 | Malformed JSON |
| `missing_provider` | 400 | No provider field |
| `invalid_provider` | 400 | Unknown provider value |
| `missing_keyword` | 400 | No keyword field |
| `invalid_keyword` | 400 | Keyword empty or >2000 chars |
| `missing_location` | 400 | No location field |
| `invalid_location` | 400 | Location invalid (zero, negative, empty string) |
| `invalid_language` | 400 | Language not 2–5 lowercase letters |
| `invalid_force_web_search` | 400 | Not a boolean |
| `invalid_field_for_provider` | 400 | force_web_search with Gemini |
| `invalid_tag` | 400 | Tag >255 chars |
| `missing_config` | 500 | Server secret not set |
| `method_not_allowed` | 405 | Wrong HTTP method |
| `not_found` | 404 | Unknown path |
| `dataforseo_timeout` | 504 | Upstream timeout-like failure detected |
| `dataforseo_error` | 502 | DataForSEO returned error |

Error response format:
```json
{
  "request_id": "a1b2c3d4-...",
  "error": "Human-readable message",
  "code": "error_code"
}
```

For `dataforseo_error` upstream failures, the worker also includes `dataforseo_status` and a UTF-8 safe, 4 KB-truncated `dataforseo_body` so callers can inspect the DataForSEO-side failure without receiving an unbounded payload. Timeout responses use the `dataforseo_timeout` code and do not include those extra upstream body fields.

## Configuration

### Secrets

| Secret | Purpose |
|--------|---------|
| `CSVKEY` | Authentication token for API clients |
| `DATAFORSEO_LOGIN` | DataForSEO API login email |
| `DATAFORSEO_PASSWORD` | DataForSEO API password |

### Environments

Configured in `wrangler.toml`:
- **Default (dev)**, `rusty-llm.<subdomain>.workers.dev`
- **Staging**, `rusty-llm-staging.<subdomain>.workers.dev`
- **Production**, `rusty-llm.<subdomain>.workers.dev` (custom domain)

### Build And Runtime Model

- The project is a `cdylib` Rust crate compiled to WebAssembly for the Cloudflare Workers runtime
- Wrangler points to `build/worker/shim.mjs`, the JS shim generated by `worker-build`
- The `[build].command` in `wrangler.toml` installs Rust, adds the `wasm32-unknown-unknown` target, installs `worker-build`, and then builds the worker
- That build command is intentionally self-contained so clean Cloudflare build environments can deploy the worker without a preinstalled Rust toolchain
- Local development and production deployment use the same Rust code path; Wrangler handles the packaging and upload steps

### Dependency Surface

The runtime dependency set is intentionally small:

- `worker` for Cloudflare Workers bindings
- `serde` and `serde_json` for structured payload work
- `base64` for upstream Basic Auth
- `getrandom` with the `js` feature for UUID entropy in WASM
- `futures` for concurrent dual-provider dispatch
- `console_error_panic_hook` for cleaner panic reporting at the edge

### Local Development

```bash
cp .dev.vars.example .dev.vars
# Fill in CSVKEY, DATAFORSEO_LOGIN, DATAFORSEO_PASSWORD
npx wrangler dev
```

Wrangler reads `.dev.vars` automatically during local development, so the local request path mirrors production secret access through `Env::secret(...)`.

### Health Endpoint Notes

- `GET /v1/health` returns JSON with version, supported providers, and whether required secrets are configured
- `HEAD /v1/health` returns headers only and sets `Content-Length` to the JSON representation size, which makes it useful for uptime probes that do not need the body
- Health checks do not touch DataForSEO and do not require a `csvkey`

## Agent Integration

### Python (LangChain)

```python
import json
import httpx

def query_llm_scraper(
    keyword: str,
    provider: str = "gemini",
    location: int = 2840,
    language: str = "en",
) -> dict:
    response = httpx.post(
        f"{WORKER_URL}/v1/llm?csvkey={CSVKEY}",
        json={
            "provider": provider,
            "keyword": keyword,
            "location": location,
            "language": language,
        },
        timeout=130.0,  # DFS can take up to 120s
    )
    response.raise_for_status()
    return response.json()
```

### LangChain Tool Definition

```python
from langchain_core.tools import tool

@tool
def llm_scraper(keyword: str, provider: str = "both") -> str:
    """Query AI search engines to see how they respond to a keyword.
    Use provider='both' to compare Gemini and ChatGPT responses."""
    result = query_llm_scraper(keyword=keyword, provider=provider)
    return json.dumps(result, indent=2)
```

### Timeout Recommendations

- Single provider: 130s (DataForSEO timeout is ~120s)
- Dual provider: 130s (concurrent, not sequential)
- Health check: 5s

### Integration Notes

- Use `provider="both"` when you want side-by-side comparison with a single correlation ID
- Pass an explicit `tag` when you want DataForSEO-side correlation to use your own workflow identifier rather than the generated request ID
- Preserve the `X-RustyLLM-Request-Id` header in downstream logs so failed or partial requests can be traced across systems
- Treat `207` as a partial success, not a hard failure; one provider response may still be usable

## Testing

### Unit Tests

```bash
cargo test
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The Rust unit suite covers:

- UUID generation format and uniqueness
- Health payload structure
- Metadata extraction from upstream JSON
- UTF-8 safe truncation for error bodies
- Provider parsing and provider capability flags
- Keyword sanitization and request validation edge cases
- DataForSEO task construction, Basic Auth generation, and dual-provider envelope assembly

### E2E Tests

```bash
./tests/e2e.sh --url "https://<worker-url>" --csvkey "<key>"
./tests/e2e.sh --url "..." --csvkey "..." --section health    # Run one section
./tests/e2e.sh --url "..." --csvkey "..." --verbose           # Show response bodies
```

The E2E harness is organized into these sections:

- `health`
- `gemini`
- `chatgpt`
- `both`
- `errors`
- `routing`
- `observability`

This gives you contract-level verification and a practical smoke test against a deployed worker.

## Deployment

### Via CLI (wrangler)

```bash
# Dev
npx wrangler deploy

# Staging
npx wrangler deploy --env staging

# Production
npx wrangler deploy --env production

# Set secrets (first deploy only)
npx wrangler secret put CSVKEY
npx wrangler secret put DATAFORSEO_LOGIN
npx wrangler secret put DATAFORSEO_PASSWORD

# Verify
curl -s "https://<worker-url>/v1/health" | jq .
```

### Via Cloudflare Dashboard (UI)

The `wrangler deploy` command handles building, uploading, and configuring the worker in one step. If you deploy with the CLI, that is enough. The steps below only cover secrets and custom domains in the dashboard.

1. **Deploy via CLI** (handles build + upload + compatibility automatically):
   ```bash
   npx wrangler deploy
   ```

2. **Configure secrets in the dashboard:**
   - Go to [Cloudflare Dashboard](https://dash.cloudflare.com) → **Workers & Pages** → **rusty-llm**
   - Navigate to **Settings** → **Variables and Secrets**
   - Add the following as **Encrypted** secrets:
     - `CSVKEY`: your chosen authentication token
     - `DATAFORSEO_LOGIN`: DataForSEO account email
     - `DATAFORSEO_PASSWORD`: DataForSEO API password

3. **Configure custom domain (optional):**
   - Go to **Settings** → **Domains & Routes**
   - Add a custom domain or note the default `rusty-llm.<subdomain>.workers.dev` URL

4. **Verify deployment:**
   ```bash
   curl -s "https://rusty-llm.<subdomain>.workers.dev/v1/health" | jq .
   ```
   Confirm `secrets_configured: true` in the response.

### Dashboard Build Settings Notes

When deploying through Cloudflare's GitHub integration:

- Leave the dashboard **Build command** empty when `wrangler.toml` already defines `[build].command`
- Use `npx wrangler deploy` as the **Deploy command**
- Leave **Root directory** empty unless the repository is genuinely nested inside a subdirectory
- Add secrets after the first deploy under **Settings → Variables and Secrets**

This keeps dashboard deploys on the same Rust and WASM packaging path used locally.

### Rollback

```bash
# Via CLI
git checkout <last-good-sha>
npx wrangler deploy --env production
```

Via dashboard: **Workers & Pages** → select worker → **Deployments** tab → click **Rollback** on a previous deployment.

## Observability

### Request Correlation

For `POST /v1/llm`, the request ID is the main correlation field:

- returned in the `X-RustyLLM-Request-Id` header
- embedded in JSON error payloads
- used as the fallback DataForSEO `tag`
- available to downstream systems as a stable correlation handle

### Structured Log Shape

Single-provider success responses and dual-provider aggregate responses emit a compact JSON log line similar to:

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

You can tail this in Wrangler or ingest it into downstream logging systems without additional transformation.

## Design Principles

1. **Error-as-value**: All functions return `Result<T, Response>` where Err IS the HTTP response
2. **Pure core, thin wrapper**: Business logic is in pure testable functions; worker-specific code is minimal
3. **Pass-through guarantee**: DataForSEO response bodies are never modified, only enriched with headers
4. **Fail fast**: Validation errors return immediately without touching secrets or making network calls
5. **Constant-time auth**: XOR fold comparison prevents timing attacks on the csvkey
6. **Stateless edge execution**: No local persistence, no session coordination, and no cross-request cache coupling
7. **Deterministic request shaping**: The same validated input always maps to the same DataForSEO payload structure
8. **Provider isolation in dual mode**: One provider failure does not automatically discard the other provider's result

## Internal Mechanics

### Keyword Sanitization Pipeline

Before dispatch, the worker strips ASCII control characters, collapses mixed whitespace into single spaces, and validates length on the normalized result. That keeps the upstream payload predictable and prevents trivial malformed-input cases from leaking into provider calls.

### Authentication Strategy

Inbound authentication is intentionally simple: the caller supplies `csvkey` as a query parameter and the worker compares it to the configured secret using a constant-time byte fold. The model is lightweight, cheap at the edge, and sufficient for controlled internal or service-to-service usage.

### Retry Algorithm

The upstream retry policy is deliberately narrow:

- first request goes to the provider endpoint immediately
- `429` and `503` trigger a 1-second delay plus exactly one retry
- other non-2xx responses fail immediately
- timeout-like failures return `504 dataforseo_timeout`

This keeps the retry policy narrow while still covering the most obvious transient upstream failures.

### Single-Provider And Dual-Provider Strategy

- **Single provider**: return the upstream JSON body as-is and add metadata in headers
- **Dual provider**: return a top-level envelope so both provider outcomes can be returned together with one overall HTTP status

The single-provider path stays close to the upstream response. The dual-provider path returns a wrapper built for side-by-side comparison.
