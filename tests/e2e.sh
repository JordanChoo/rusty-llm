#!/usr/bin/env bash
set -euo pipefail

PASS=0
FAIL=0
SKIP=0
VERBOSE=false
SECTION=""
BASE_URL=""
CSVKEY=""
LOG_FILE="tests/e2e-$(date +%Y%m%d-%H%M%S).log"

usage() {
    echo "Usage: $0 --url <worker-url> --csvkey <key> [--verbose] [--section <name>]"
    echo ""
    echo "Sections: health, gemini, chatgpt, both, errors, routing, observability"
    exit 1
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --url) BASE_URL="$2"; shift 2 ;;
        --csvkey) CSVKEY="$2"; shift 2 ;;
        --verbose) VERBOSE=true; shift ;;
        --section) SECTION="$2"; shift 2 ;;
        *) usage ;;
    esac
done

[[ -z "$BASE_URL" ]] && usage
[[ -z "$CSVKEY" ]] && usage

BASE_URL="${BASE_URL%/}"

check_json_field() {
    local file="$1" field="$2" expected="$3"
    local actual
    actual=$(jq -r "$field" "$file" 2>/dev/null)
    if [[ "$actual" != "$expected" ]]; then
        echo "    Expected $field = '$expected', got '$actual'" >> "$LOG_FILE"
        return 1
    fi
}

check_header() {
    local header_file="$1" header_name="$2"
    local value
    value=$(grep -i "^${header_name}:" "$header_file" | head -1 | sed "s/^[^:]*: //" | tr -d '\r\n')
    if [[ -z "$value" ]]; then
        echo "    Header $header_name missing" >> "$LOG_FILE"
        return 1
    fi
    echo "$value"
}

check_uuid_v4() {
    local id="$1"
    if echo "$id" | grep -qE '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'; then
        return 0
    fi
    echo "    Not valid UUID v4: $id" >> "$LOG_FILE"
    return 1
}

test_case() {
    local name="$1" expected_status="$2" method="$3" path="$4"
    local body="${5:-}" extra_checks="${6:-}"

    printf "  [%-28s] " "$name"

    local response_file header_file http_status
    response_file=$(mktemp)
    header_file=$(mktemp)

    if [[ -n "$body" ]]; then
        http_status=$(curl -s -o "$response_file" -D "$header_file" \
            -w "%{http_code}" -X "$method" \
            "${BASE_URL}${path}" \
            -H "Content-Type: application/json" \
            -d "$body" 2>/dev/null)
    else
        http_status=$(curl -s -o "$response_file" -D "$header_file" \
            -w "%{http_code}" -X "$method" \
            "${BASE_URL}${path}" 2>/dev/null)
    fi

    {
        echo "=== TEST: $name ==="
        echo "Request: $method ${BASE_URL}${path}"
        [[ -n "$body" ]] && echo "Body: $body"
        echo "Expected status: $expected_status"
        echo "Actual status: $http_status"
        echo "Response headers:"
        cat "$header_file"
        echo "Response body:"
        cat "$response_file"
        echo ""
    } >> "$LOG_FILE"

    if [[ "$http_status" != "$expected_status" ]]; then
        printf "\033[31mFAIL\033[0m (expected %s, got %s)\n" "$expected_status" "$http_status"
        [[ "$VERBOSE" == true ]] && jq . "$response_file" 2>/dev/null || true
        FAIL=$((FAIL + 1))
        rm -f "$response_file" "$header_file"
        return 1
    fi

    if [[ -n "$extra_checks" ]]; then
        if ! "$extra_checks" "$response_file" "$header_file"; then
            printf "\033[31mFAIL\033[0m (validation failed)\n"
            FAIL=$((FAIL + 1))
            rm -f "$response_file" "$header_file"
            return 1
        fi
    fi

    printf "\033[32mPASS\033[0m\n"
    PASS=$((PASS + 1))
    rm -f "$response_file" "$header_file"
    return 0
}

# --- Validation functions ---

check_health_get() {
    local resp="$1"
    check_json_field "$resp" ".status" "ok" || return 1
    check_json_field "$resp" ".secrets_configured" "true" || return 1
    local providers
    providers=$(jq -r '.providers | length' "$resp" 2>/dev/null)
    [[ "$providers" == "3" ]] || return 1
}

check_health_head_no_body() {
    local resp="$1"
    local size
    size=$(wc -c < "$resp")
    [[ "$size" -eq 0 ]] || { echo "    HEAD response has body ($size bytes)" >> "$LOG_FILE"; return 1; }
}

check_gemini_response() {
    local resp="$1" headers="$2"
    local ct
    ct=$(check_header "$headers" "content-type") || return 1
    echo "$ct" | grep -qi "application/json" || return 1
}

check_both_envelope() {
    local resp="$1"
    jq -e '.gemini' "$resp" >/dev/null 2>&1 || return 1
    jq -e '.chatgpt' "$resp" >/dev/null 2>&1 || return 1
    jq -e '.request_id' "$resp" >/dev/null 2>&1 || return 1
}

check_error_code() {
    local expected_code="$1"
    return_func() {
        local resp="$2"
        check_json_field "$resp" ".code" "$expected_code"
    }
    echo "return_func"
}

check_request_id_header() {
    local resp="$1" headers="$2"
    local rid
    rid=$(check_header "$headers" "x-rustyLLM-request-id") || rid=$(check_header "$headers" "X-RustyLLM-Request-Id") || return 1
    check_uuid_v4 "$rid"
}

check_metadata_headers() {
    local resp="$1" headers="$2"
    check_header "$headers" "X-RustyLLM-Request-Id" >/dev/null || return 1
    check_header "$headers" "X-RustyLLM-Provider" >/dev/null || return 1
    check_header "$headers" "X-RustyLLM-Duration-Ms" >/dev/null || return 1
    check_header "$headers" "X-RustyLLM-Retried" >/dev/null || return 1
    check_header "$headers" "X-RustyLLM-DFS-Status" >/dev/null || return 1
}

check_error_has_request_id() {
    local resp="$1" headers="$2"
    local header_rid body_rid
    header_rid=$(check_header "$headers" "X-RustyLLM-Request-Id") || return 1
    body_rid=$(jq -r '.request_id' "$resp" 2>/dev/null)
    [[ "$header_rid" == "$body_rid" ]] || { echo "    Header rid '$header_rid' != body rid '$body_rid'" >> "$LOG_FILE"; return 1; }
}

# --- Test Sections ---

run_health() {
    echo ""
    echo "[Health Check]"
    test_case "health_get" "200" "GET" "/v1/health" "" "check_health_get" || true
    test_case "health_head" "200" "HEAD" "/v1/health" "" "check_health_head_no_body" || true
    test_case "health_post" "405" "POST" "/v1/health" "" "" || true
}

run_gemini() {
    echo ""
    echo "[Gemini Provider]"
    local body='{"provider":"gemini","keyword":"best coffee shops","location":2840}'
    test_case "gemini_basic" "200" "POST" "/v1/llm?csvkey=${CSVKEY}" "$body" "check_gemini_response" || true

    body='{"provider":"gemini","keyword":"best restaurants","location":"United States"}'
    test_case "gemini_location_name" "200" "POST" "/v1/llm?csvkey=${CSVKEY}" "$body" "" || true

    body='{"provider":"gemini","keyword":"test query","location":2840}'
    test_case "gemini_defaults" "200" "POST" "/v1/llm?csvkey=${CSVKEY}" "$body" "" || true

    body='{"provider":"gemini","keyword":"tagged query","location":2840,"tag":"e2e-test"}'
    test_case "gemini_custom_tag" "200" "POST" "/v1/llm?csvkey=${CSVKEY}" "$body" "" || true
}

run_chatgpt() {
    echo ""
    echo "[ChatGPT Provider]"
    local body='{"provider":"chatgpt","keyword":"best coffee shops","location":2840}'
    test_case "chatgpt_basic" "200" "POST" "/v1/llm?csvkey=${CSVKEY}" "$body" "" || true

    body='{"provider":"chatgpt","keyword":"latest news","location":2840,"force_web_search":true}'
    test_case "chatgpt_web_search" "200" "POST" "/v1/llm?csvkey=${CSVKEY}" "$body" "" || true

    body='{"provider":"chatgpt","keyword":"test query","location":2840,"force_web_search":false}'
    test_case "chatgpt_no_search" "200" "POST" "/v1/llm?csvkey=${CSVKEY}" "$body" "" || true
}

run_both() {
    echo ""
    echo "[Dual Provider \"both\"]"
    local body='{"provider":"both","keyword":"best coffee shops","location":2840}'
    test_case "both_basic" "200" "POST" "/v1/llm?csvkey=${CSVKEY}" "$body" "check_both_envelope" || true

    body='{"provider":"both","keyword":"latest news","location":2840,"force_web_search":true}'
    test_case "both_web_search" "200" "POST" "/v1/llm?csvkey=${CSVKEY}" "$body" "" || true

    body='{"provider":"both","keyword":"concurrency test","location":2840}'
    local start_ms=$(($(date +%s%N)/1000000))
    test_case "both_concurrency" "200" "POST" "/v1/llm?csvkey=${CSVKEY}" "$body" "" || true
    local end_ms=$(($(date +%s%N)/1000000))
    local elapsed=$((end_ms - start_ms))
    echo "    (elapsed: ${elapsed}ms — should be ~max(gemini,chatgpt), not sum)"
}

run_errors() {
    echo ""
    echo "[Error Cases]"
    local body

    test_case "err_missing_csvkey" "400" "POST" "/v1/llm" '{"provider":"gemini","keyword":"test","location":2840}' "" || true
    test_case "err_wrong_csvkey" "401" "POST" "/v1/llm?csvkey=wrong-key" '{"provider":"gemini","keyword":"test","location":2840}' "" || true
    test_case "err_empty_body" "400" "POST" "/v1/llm?csvkey=${CSVKEY}" "" "" || true
    test_case "err_invalid_json" "400" "POST" "/v1/llm?csvkey=${CSVKEY}" "{not json" "" || true
    test_case "err_missing_provider" "400" "POST" "/v1/llm?csvkey=${CSVKEY}" '{"keyword":"test","location":2840}' "" || true
    test_case "err_invalid_provider" "400" "POST" "/v1/llm?csvkey=${CSVKEY}" '{"provider":"perplexity","keyword":"test","location":2840}' "" || true
    test_case "err_missing_keyword" "400" "POST" "/v1/llm?csvkey=${CSVKEY}" '{"provider":"gemini","location":2840}' "" || true
    test_case "err_empty_keyword" "400" "POST" "/v1/llm?csvkey=${CSVKEY}" '{"provider":"gemini","keyword":"   ","location":2840}' "" || true

    local long_kw
    long_kw=$(printf 'x%.0s' $(seq 1 2001))
    test_case "err_long_keyword" "400" "POST" "/v1/llm?csvkey=${CSVKEY}" "{\"provider\":\"gemini\",\"keyword\":\"${long_kw}\",\"location\":2840}" "" || true

    test_case "err_missing_location" "400" "POST" "/v1/llm?csvkey=${CSVKEY}" '{"provider":"gemini","keyword":"test"}' "" || true
    test_case "err_negative_location" "400" "POST" "/v1/llm?csvkey=${CSVKEY}" '{"provider":"gemini","keyword":"test","location":-5}' "" || true
    test_case "err_empty_location" "400" "POST" "/v1/llm?csvkey=${CSVKEY}" '{"provider":"gemini","keyword":"test","location":""}' "" || true
    test_case "err_bad_language" "400" "POST" "/v1/llm?csvkey=${CSVKEY}" '{"provider":"gemini","keyword":"test","location":2840,"language":"English"}' "" || true
    test_case "err_force_ws_gemini" "400" "POST" "/v1/llm?csvkey=${CSVKEY}" '{"provider":"gemini","keyword":"test","location":2840,"force_web_search":true}' "" || true
    test_case "err_force_ws_not_bool" "400" "POST" "/v1/llm?csvkey=${CSVKEY}" '{"provider":"chatgpt","keyword":"test","location":2840,"force_web_search":"true"}' "" || true

    local long_tag
    long_tag=$(printf 'x%.0s' $(seq 1 256))
    test_case "err_tag_too_long" "400" "POST" "/v1/llm?csvkey=${CSVKEY}" "{\"provider\":\"gemini\",\"keyword\":\"test\",\"location\":2840,\"tag\":\"${long_tag}\"}" "" || true
}

run_routing() {
    echo ""
    echo "[Routing]"
    test_case "route_404" "404" "GET" "/v1/serp" "" "" || true
    test_case "route_405_get_llm" "405" "GET" "/v1/llm" "" "" || true
    test_case "route_405_put_llm" "405" "PUT" "/v1/llm" "" "" || true
}

run_observability() {
    echo ""
    echo "[Observability]"
    local body='{"provider":"gemini","keyword":"observability test","location":2840}'

    local resp1 header1
    resp1=$(mktemp)
    header1=$(mktemp)
    curl -s -o "$resp1" -D "$header1" -X POST \
        "${BASE_URL}/v1/llm?csvkey=${CSVKEY}" \
        -H "Content-Type: application/json" \
        -d "$body" 2>/dev/null

    local rid1
    rid1=$(check_header "$header1" "X-RustyLLM-Request-Id" 2>/dev/null) || rid1=""

    printf "  [%-28s] " "obs_request_id_format"
    if [[ -n "$rid1" ]] && check_uuid_v4 "$rid1"; then
        printf "\033[32mPASS\033[0m\n"
        PASS=$((PASS + 1))
    else
        printf "\033[31mFAIL\033[0m (rid='%s')\n" "$rid1"
        FAIL=$((FAIL + 1))
    fi

    local resp2 header2
    resp2=$(mktemp)
    header2=$(mktemp)
    curl -s -o "$resp2" -D "$header2" -X POST \
        "${BASE_URL}/v1/llm?csvkey=${CSVKEY}" \
        -H "Content-Type: application/json" \
        -d "$body" 2>/dev/null

    local rid2
    rid2=$(check_header "$header2" "X-RustyLLM-Request-Id" 2>/dev/null) || rid2=""

    printf "  [%-28s] " "obs_request_id_unique"
    if [[ -n "$rid1" && -n "$rid2" && "$rid1" != "$rid2" ]]; then
        printf "\033[32mPASS\033[0m\n"
        PASS=$((PASS + 1))
    else
        printf "\033[31mFAIL\033[0m (rid1='%s', rid2='%s')\n" "$rid1" "$rid2"
        FAIL=$((FAIL + 1))
    fi

    local err_resp err_header
    err_resp=$(mktemp)
    err_header=$(mktemp)
    curl -s -o "$err_resp" -D "$err_header" -X POST \
        "${BASE_URL}/v1/llm?csvkey=${CSVKEY}" \
        -H "Content-Type: application/json" \
        -d '{"provider":"gemini","keyword":"test"}' 2>/dev/null

    printf "  [%-28s] " "obs_request_id_in_error"
    local err_rid err_body_rid
    err_rid=$(check_header "$err_header" "X-RustyLLM-Request-Id" 2>/dev/null) || err_rid=""
    err_body_rid=$(jq -r '.request_id' "$err_resp" 2>/dev/null) || err_body_rid=""
    if [[ -n "$err_rid" && "$err_rid" == "$err_body_rid" ]]; then
        printf "\033[32mPASS\033[0m\n"
        PASS=$((PASS + 1))
    else
        printf "\033[31mFAIL\033[0m (header='%s', body='%s')\n" "$err_rid" "$err_body_rid"
        FAIL=$((FAIL + 1))
    fi

    printf "  [%-28s] " "obs_metadata_headers"
    if check_metadata_headers "$resp1" "$header1" 2>/dev/null; then
        printf "\033[32mPASS\033[0m\n"
        PASS=$((PASS + 1))
    else
        printf "\033[31mFAIL\033[0m\n"
        FAIL=$((FAIL + 1))
    fi

    printf "  [%-28s] " "obs_metadata_values"
    local duration dfs_status
    duration=$(check_header "$header1" "X-RustyLLM-Duration-Ms" 2>/dev/null) || duration="0"
    dfs_status=$(check_header "$header1" "X-RustyLLM-DFS-Status" 2>/dev/null) || dfs_status=""
    if [[ "$duration" -gt 0 && -n "$dfs_status" ]]; then
        printf "\033[32mPASS\033[0m\n"
        PASS=$((PASS + 1))
    else
        printf "\033[31mFAIL\033[0m (duration=%s, dfs_status=%s)\n" "$duration" "$dfs_status"
        FAIL=$((FAIL + 1))
    fi

    rm -f "$resp1" "$header1" "$resp2" "$header2" "$err_resp" "$err_header"
}

# --- Main ---

echo ""
echo "╔══════════════════════════════════════════════╗"
echo "║     rusty-llm E2E Test Suite                 ║"
printf "║     URL: %-35s║\n" "$BASE_URL"
printf "║     Time: %-34s║\n" "$(date -u '+%Y-%m-%d %H:%M:%S UTC')"
echo "╚══════════════════════════════════════════════╝"

touch "$LOG_FILE"

if [[ -z "$SECTION" ]]; then
    run_health
    run_gemini
    run_chatgpt
    run_both
    run_errors
    run_routing
    run_observability
else
    case "$SECTION" in
        health) run_health ;;
        gemini) run_gemini ;;
        chatgpt) run_chatgpt ;;
        both) run_both ;;
        errors) run_errors ;;
        routing) run_routing ;;
        observability) run_observability ;;
        *) echo "Unknown section: $SECTION"; usage ;;
    esac
fi

echo ""
echo "══════════════════════════════════════════════"
echo "  RESULTS: ${PASS} passed, ${FAIL} failed, ${SKIP} skipped"
echo "  Log file: ${LOG_FILE}"
echo "══════════════════════════════════════════════"
echo ""

exit "$FAIL"
