# rusty-llm

Rust Cloudflare Worker that proxies DataForSEO's AI Optimization LLM Scraper endpoints (Gemini and ChatGPT). Provides unified request validation, dual-provider concurrent fetching, automatic retry on transient errors, metadata header enrichment, and structured logging.

## Key Features

- **Dual-provider mode**: Query Gemini and ChatGPT concurrently with a single request
- **Metadata headers**: Model name, cost, DFS status, duration, retry flag on every response
- **Request IDs**: UUID v4 for every request, in both headers and error bodies
- **Automatic retry**: Single retry on 429/503 from DataForSEO
- **Input validation**: Strict field validation with descriptive error codes
- **Structured logging**: One JSON log line per request via `console_log`

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
- `errors.rs` — JSON error response builders, UTF-8 safe truncation
- `providers.rs` — Provider enum, URL constants, per-provider feature flags
- `validation.rs` — Auth, body parsing, field validation, input sanitization
- `dataforseo.rs` — HTTP client, retry logic, dual-provider envelope

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

#### Request Fields

| Field | Type | Required | Default | Constraints |
|-------|------|----------|---------|-------------|
| `provider` | string | yes | — | `"gemini"`, `"chatgpt"`, or `"both"` |
| `keyword` | string | yes | — | 1–2000 chars after sanitization |
| `location` | int or string | yes | — | Positive integer (DFS code) or non-empty string (location name) |
| `language` | string | no | `"en"` | 2–5 lowercase letters |
| `force_web_search` | boolean | no | `false` | Only valid for `chatgpt` and `both` |
| `tag` | string | no | — | Max 255 chars |

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

#### Single-Provider Response

Returns the DataForSEO response body directly with metadata headers attached.

#### Dual-Provider Response ("both" mode)

Returns an envelope:
```json
{
  "request_id": "a1b2c3d4-...",
  "total_duration_ms": 4500,
  "gemini": {
    "status": "ok",
    "duration_ms": 3200,
    "model": "gemini-2.0-flash",
    "dfs_cost_cents": 4,
    "response": { ... }
  },
  "chatgpt": {
    "status": "ok",
    "duration_ms": 4500,
    "model": "gpt-4o",
    "dfs_cost_cents": 4,
    "response": { ... }
  }
}
```

HTTP status codes for dual mode:
- `200` — Both providers succeeded
- `207` — One provider failed (partial success)
- `502` — Both providers failed

### Metadata Headers

Present on all successful single-provider responses:

| Header | Description | Example |
|--------|-------------|---------|
| `X-RustyLLM-Request-Id` | UUID v4 request identifier | `a1b2c3d4-e5f6-...` |
| `X-RustyLLM-Provider` | Provider used | `gemini` |
| `X-RustyLLM-Model` | LLM model from DFS response | `gemini-2.0-flash` |
| `X-RustyLLM-Duration-Ms` | Total processing time | `4200` |
| `X-RustyLLM-DFS-Cost-Cents` | DataForSEO cost in cents | `4` |
| `X-RustyLLM-DFS-Status` | DataForSEO task status code | `20000` |
| `X-RustyLLM-Retried` | Whether a retry was triggered | `false` |

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
| `dataforseo_error` | 502 | DataForSEO returned error |

Error response format:
```json
{
  "request_id": "a1b2c3d4-...",
  "error": "Human-readable message",
  "code": "error_code"
}
```

## Configuration

### Secrets

| Secret | Purpose |
|--------|---------|
| `CSVKEY` | Authentication token for API clients |
| `DATAFORSEO_LOGIN` | DataForSEO API login email |
| `DATAFORSEO_PASSWORD` | DataForSEO API password |

### Environments

Configured in `wrangler.toml`:
- **Default (dev)**: `rusty-llm.<subdomain>.workers.dev`
- **Staging**: `rusty-llm-staging.<subdomain>.workers.dev`
- **Production**: `rusty-llm.<subdomain>.workers.dev` (custom domain)

### Local Development

```bash
cp .dev.vars.example .dev.vars
# Fill in CSVKEY, DATAFORSEO_LOGIN, DATAFORSEO_PASSWORD
npx wrangler dev
```

## Agent Integration

### Python (LangChain)

```python
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

## Testing

### Unit Tests

```bash
cargo test           # 105 tests
cargo clippy --target wasm32-unknown-unknown  # Lint
cargo fmt --check    # Format verification
```

### E2E Tests

```bash
./tests/e2e.sh --url "https://<worker-url>" --csvkey "<key>"
./tests/e2e.sh --url "..." --csvkey "..." --section health    # Run one section
./tests/e2e.sh --url "..." --csvkey "..." --verbose           # Show response bodies
```

## Deployment

```bash
# Dev
npx wrangler deploy

# Staging
npx wrangler deploy --env staging

# Production
npx wrangler deploy --env production

# Verify
curl -s "https://<worker-url>/v1/health" | jq .
```

### Rollback

```bash
git checkout <last-good-sha>
npx wrangler deploy --env production
```

## Design Principles

1. **Error-as-value**: All functions return `Result<T, Response>` where Err IS the HTTP response
2. **Pure core, thin wrapper**: Business logic is in pure testable functions; worker-specific code is minimal
3. **Pass-through guarantee**: DataForSEO response bodies are never modified, only enriched with headers
4. **Fail fast**: Validation errors return immediately without touching secrets or making network calls
5. **Constant-time auth**: XOR fold comparison prevents timing attacks on the csvkey
