# curl builtin — design notes

80/20 of real curl with better defaults for AI agents. Deferred from the
initial builtin batch due to `reqwest` dependency weight.

## Interface

```
curl [OPTIONS] URL
```

### Flags

| Flag | Description | Default |
|------|-------------|---------|
| `-X METHOD` | HTTP method | GET (auto-POST when `-d` present) |
| `-H "Key: Value"` | Request header (repeatable) | — |
| `-d BODY` | Request body (`@file` reads from VFS) | — |
| `-o FILE` | Write response body to VFS file | — |
| `-f` | Fail on HTTP 4xx/5xx (exit 22) | off |
| `-i` | Include status line + headers in output | off |
| `-L` / `--location` | Follow redirects | **ON** (unlike real curl) |
| `--no-location` | Disable redirect following | — |
| `-s` | Silent — accepted but no-op (always silent) | — |
| `--max-redirects N` | Redirect limit | 10 |

### Better defaults vs real curl

- Follow redirects: ON by default
- No progress output: always
- JSON body auto-detection: if body starts with `{`/`[` and no
  Content-Type header set, adds `application/json`
- `-d` with no `-X` promotes method to POST (same as real curl)

### `--json` output

Uses `OutputData::table` with columns `STATUS`, `HEADER`, `BODY`:

```json
[{"STATUS": "200", "HEADER": "{\"content-type\": \"application/json\"}", "BODY": "..."}]
```

Gives agents structured access to HTTP response metadata without parsing.

## Dependencies

```toml
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }
wiremock = "0.6"  # dev-dep for tests
```

`reqwest` pulls in hyper, tower, rustls, webpki-roots — substantial compile
time impact. Consider gating behind `features = ["curl"]` (default on).

## Testing

Use `wiremock` for local test HTTP server. No network-dependent tests.

- `test_curl_missing_url` — error on no URL
- `test_curl_header_parsing` — `-H` flag handling
- `test_curl_body_from_file` — `-d @file` reads VFS
- `test_curl_auto_content_type` — JSON body detection
- `test_curl_method_promotion` — GET with `-d` becomes POST
- `test_curl_get` — basic GET via wiremock
- `test_curl_post_json` — POST with body
- `test_curl_fail_on_error` — `-f` with 4xx
- `test_curl_follow_redirect` — redirect following
- `test_curl_output_file` — `-o` writes to VFS
- `test_curl_include_headers` — `-i` flag

## Known limitations (v1)

- **Buffered responses**: entire body buffered in memory, no streaming
- **Text only**: binary response bodies go through `from_utf8_lossy()`
- **No auth flags**: no `-u user:pass`, no `--bearer`. Use `-H "Authorization: ..."` instead
- **No TLS config**: no `--cacert`, `--cert`, `-k`. Use real curl for those
- **VFS paths only**: `-d @file` and `-o file` go through VFS, not raw filesystem
