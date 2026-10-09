# Rust backend

This service is the Rust backend being split from the Next.js dashboard. API
coverage is being migrated incrementally; endpoints not listed as implemented
here remain on the Node.js backend.

## Run locally

```sh
cd backend-rust
cargo run
```

The service listens on `0.0.0.0:8080` by default. Set `PORT` to override it.
Set `VANSROUTER_VERSION` to the dashboard release version and optionally set
`RELEASE_BUILD_ID` when deploying.

The Rust sources are split by responsibility: `main.rs` wires the service and
shared request/chat logic, `admin.rs` contains dashboard CRUD endpoints,
`usage.rs` contains usage-report endpoints, and `auth.rs`/`oidc.rs` handle
dashboard authentication. Slow login requests log total time and any settings
read or bcrypt verification phase taking at least 500 ms; check the Rust
service logs to identify which stage is delaying login.

Endpoints:

- `GET /health`
- `GET /api/health` (same response, for compatibility with the existing API path)
- `GET /api/init` (dashboard initialization probe)
- `GET /api/ready` (persistent SQLite readiness check)
- `GET /api/version` (version/update data for the dashboard)
- `POST /api/locale` (validated locale preference cookie)
- `GET/PATCH/DELETE /api/pricing` (merged catalog pricing and user overrides)
- `POST /api/auth/login`, `GET /api/auth/status`, `GET /api/auth/check`,
  `POST /api/auth/logout`, and `POST /api/auth/reset-password`
- `GET /api/settings/require-login` (dashboard and tunnel access settings)
- `GET /api/usage/providers` (providers present in request-detail history)
- `GET/PUT /api/models` (static dashboard model list and model aliases)
- `GET/OPTIONS /v1beta/models` (Gemini-compatible model list)
- `GET /api/tags` (Ollama-compatible static model list)
- `GET /v1/models` (static, connected, custom, and combo LLM models)
- `POST /v1/chat/completions` (OpenAI and custom OpenAI-compatible-node calls)
- `GET/POST /api/providers` and `GET/PUT/DELETE /api/providers/{id}`
  (API-key catalog and custom-node connections)
- `GET/POST /api/keys` and `GET/PUT/DELETE /api/keys/{id}`
  (dashboard API-key management)
- `GET/POST /api/provider-nodes` and `PUT/DELETE /api/provider-nodes/{id}`
  (custom provider-node management)
- `GET/POST /api/combos` and `GET/PUT/DELETE /api/combos/{id}` (combo CRUD)
- `GET/PUT/DELETE /api/models/alias` (model alias configuration)
- `GET/POST/DELETE /api/models/custom` (custom model configuration)
- `GET/POST/DELETE /api/models/disabled` (disabled model configuration)

Set `DATA_DIR` to the same persistent data directory used by the existing
gateway; otherwise the service looks for `$HOME/.9router/db/data.sqlite`.
Provider and combo CRUD writes directly to this SQLite database.
API-key CRUD uses the same database and persists rate-limit cleanup when keys
are deleted. Creating a key requires the shared `machine-id` file to have
been initialized by the existing Node.js service; configure the same
`MACHINE_ID_SALT` and `API_KEY_SECRET` on both services when those environment
variables are set. Otherwise, Rust reads or creates the persistent
`auth/api-key-secret` file under `DATA_DIR`.
Provider CRUD accepts catalog ids, aliases, and display names, API-key-capable
catalog providers, and configured custom provider nodes. It preserves provider
priority, proxy settings, proxy-pool assignments, provider-specific settings,
and the Node API's same-name API-key connection upsert behavior. OAuth imports,
credential refresh, provider tests, provider-node validation, and proxy-pool
administration remain on Node.js. Provider-node create/update/delete keeps
linked provider-connection settings synchronized and deletes linked
connections when a node is removed.
It validates API keys and applies provider, combo, and kind ACLs. The endpoint
lists static catalog entries, active provider selections, custom models, model
aliases, disabled models, and combos. Dynamic upstream model discovery and live
OAuth model resolvers are not included in this first Rust slice.
Remote requests follow the gateway's public API policy: valid API keys and the
persisted CLI token are accepted; anonymous remote requests are accepted only
when `allowRemoteNoApiKey` is enabled and API-key enforcement is disabled.
Loopback requests retain local access unless explicitly marked as proxied.

Regenerate the bundled static catalog after changing provider model registries:

```sh
node backend-rust/scripts/export-model-catalog.mjs
```

Set `NEXT_PUBLIC_RUST_API_URL` to the Rust service URL and
`RUST_BE=true` to route the implemented paths to Rust. The `/v1/models` and
`/api/tags` rewrites use that single flag. `/v1/chat/completions` is routed
through the Next.js API handler: only explicit `openai/<model>` requests go to
Rust for now, while other models stay on the existing Node implementation.
Rust chat requires an active API-key connection in the shared SQLite database.
Responses API nodes and other catalog providers, including
Anthropic-compatible, OAuth-backed, and specialized-executor providers, are
not yet ported.
The endpoint preserves upstream SSE bytes, validates the gateway API key and
model/provider ACL, applies API-key input/output token limits and requests per
minute limits, and sends the provider credential only to the configured
OpenAI-compatible upstream. Direct Rust chat calls reject API keys with an IP
whitelist until trusted client-IP forwarding is implemented. Through Next.js,
the exact unsupported-IP response falls back to the existing Node handler.

`RUST_BE=true` also routes the implemented API-key, provider-connection, custom
provider-node, combo, alias, custom-model, disabled-model, and static model-list
handlers to Rust. OAuth flows, provider-node validation, presets, dynamic
model-fetcher results, testing, and provider-specific subroutes are not yet
ported. Rust verifies the
dashboard `auth_token` JWT using `JWT_SECRET` (or the validated CLI token), so configure the exact same stable
`JWT_SECRET` in the Next.js/Vercel runtime and Rust backend. Do not enable the
flag until both services share that secret and the same persistent `DATA_DIR`.
Successful combo writes through the Next.js proxy invalidate its in-process
allowed-model cache and reset combo rotation state. Direct combo writes to the
Rust service bypass those process-local Node invalidations; route dashboard
traffic through Next.js unless all Node gateway processes are restarted or a
shared invalidation mechanism is deployed.

The Rust dashboard model list uses the generated static LLM catalog and shared
SQLite alias/disabled-model settings. Gemini model discovery uses a generated
all-kinds catalog. Dynamic model-fetcher results for no-auth providers are not
yet included in `/api/models`; requests continue to use the static list when
Rust routing is enabled.

The same `RUST_BE=true` flag routes dashboard password login, auth status
and checks, logout, first-password setup, and OIDC start/callback/testing
through Next.js to Rust. Rust performs OIDC discovery, authorization-code
exchange with PKCE, state/nonce checks, JWKS signature verification, and
dashboard-session creation. Password and OIDC tokens use the same `JWT_SECRET`
as Next.js, or the same persistent
`DATA_DIR/jwt-secret` file when no environment secret is configured. Password
updates are written to the shared SQLite settings row using bcrypt, and the
Rust response cookies are passed back through Next.js. Configure a stable
shared secret and persistent database before enabling this flag. Provider OAuth
connection flows and the remaining API routes are still on Node.js.

Health endpoints return `{"ok":true}`. The version endpoint returns
`currentVersion`, `latestVersion`, `hasUpdate`, and `buildId`, matching the
existing Next.js route. CORS currently allows any origin; protected API routes
still enforce their own API-key policy.

## Deployment direction

Run this service on a persistent backend host and expose it over HTTPS, for
example as `https://api.example.com`. Set `NEXT_PUBLIC_RUST_API_URL` and
`RUST_BE=true` in the frontend deployment. Node and Rust can access the same
SQLite state only when both run where the same persistent `DATA_DIR` is mounted.
A Vercel function filesystem is not that shared persistent storage: do not
enable mixed Node/Rust writes with a Vercel-local SQLite file. The project is
not yet a Rust-only backend: provider OAuth and many API routes remain on
Node.js. Keep `RUST_BE=false` for deployments that still depend on Node routes
unless they have a separately verified shared data service.

The Rust chat backend embeds `pricing.json`. Regenerate it from the shared
pricing source after changing model rates or pricing patterns:

```sh
node backend-rust/scripts/sync-pricing.mjs
```
