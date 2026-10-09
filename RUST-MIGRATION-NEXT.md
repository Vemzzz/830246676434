# Rust migration handoff

Status recorded: 2026-10-08

## User's priority

The next migration work should prioritize:

1. Provider connection CRUD used by the dashboard.
2. Combo CRUD used by the dashboard.
3. `POST /v1/chat/completions`.

The goal is to host the Next.js frontend on Vercel and run the Rust backend on a
persistent server. Do not remove the Node routes yet: they remain the fallback
for features not ported to Rust.

Current route-file snapshot: **46/172 Rust-backed (26.7%), 126 remaining**.
See [RUST-ROUTE-CHECKLIST.md](./RUST-ROUTE-CHECKLIST.md) for the complete
inventory; this is not a completed migration.

## Implemented in the current worktree

- Rust Axum service in `backend-rust/`, reading the existing SQLite database
  under `DATA_DIR/db/data.sqlite`.
- `GET /v1/models` and an initial `POST /v1/chat/completions` implementation.
- Chat supports `openai/<model>` through OpenAI and `openai-compatible` chat
  nodes, forwards upstream SSE, applies API-key limits, and fails closed for
  IP-whitelisted API keys. Other provider types still need Node.
- Rust provider-connection routes:
  - `GET/POST /api/providers`
  - `GET/PUT/DELETE /api/providers/{id}`
- Rust combo routes:
  - `GET/POST /api/combos`
  - `GET/PUT/DELETE /api/combos/{id}`
- Rust API-key routes:
  - `GET/POST /api/keys`
  - `GET/PUT/DELETE /api/keys/{id}`
- Rust custom provider-node routes:
  - `GET/POST /api/provider-nodes`
  - `PUT/DELETE /api/provider-nodes/{id}`
- Rust password-auth routes:
  - `POST /api/auth/login`
  - `GET /api/auth/status`
  - `GET /api/auth/check`
  - `POST /api/auth/logout`
  - `POST /api/auth/reset-password`
- Rust OIDC routes:
  - `GET /api/auth/oidc/start`
  - `GET /api/auth/oidc/callback`
  - `POST /api/auth/oidc/test`
- Admin routes require a valid dashboard `auth_token` JWT signed with
  `JWT_SECRET`, or the persisted CLI token.
- Next.js route handlers proxy API-key, provider, custom provider-node, and
  combo collection/resource paths to Rust when `RUST_BE=true`.
- Password and OIDC auth routes proxy to Rust when `RUST_BE=true`.
- API-key CRUD preserves Node key generation, limits/IP normalization, and
  rate-limit cleanup on deletion. Key creation reads the shared machine ID and
  uses the configured or persistent API-key secret.
- Provider-node create/update/delete preserves Node URL normalization and
  keeps linked provider connection settings synchronized; deletion removes
  linked connections in the same transaction.
- Chat routing uses the same `RUST_BE=true` switch and proxies only models
  currently supported by the Rust handler; unsupported models stay on Node.js.
- Rust chat resolves configured combos and applies fallback/round-robin among
  supported OpenAI-compatible members.
- Rust implements `/api/usage/history`, `/api/usage/stats`, and
  `/api/usage/chart`; the corresponding Next.js routes proxy to Rust when
  `RUST_BE=true`.
- Rust implements `/api/models/availability` to report active model cooldowns
  and clear a model lock; the Next.js route proxies reads and mutations to Rust
  when `RUST_BE=true`.
- Rust implements authenticated `/api/providers/{id}/overrides` reads and
  writes with registry built-in headers and Node-compatible header validation;
  the Next.js route proxies when `RUST_BE=true`.
- Rust implements paginated, filterable `/api/usage/request-details` reads and
  `/api/usage/request-details/{id}`; list results retain Node's request/response
  redaction behavior, and the routes proxy when `RUST_BE=true`.
- Rust implements `/api/usage/logs` and legacy `/api/usage/request-logs` from
  `usageHistory`; both Next routes proxy when `RUST_BE=true`.
- Rust implements `GET/PUT /api/models` for the generated static LLM catalog,
  stored aliases, and disabled-model settings. Model alias writes through
  Next.js invalidate the local allowed-model cache.
- Rust implements `GET/OPTIONS /v1beta/models` using the generated all-kinds
  model catalog and Gemini-compatible response shape.
- The Next.js `/api/v1/models` and `/api/tags` routes now proxy to their Rust
  equivalents when `RUST_BE=true`; both retain their existing Node handlers as
  the fallback.
- The model catalog exporter retains the existing LLM-only `models` list and
  additionally generates compact `allModels` entries for Gemini discovery.
- Rust implements `GET/OPTIONS /v1/models/info` with catalog metadata, model
  aliases, virtual search/fetch entries, and CORS-compatible responses. The
  Next.js route proxies to Rust when `RUST_BE=true` and retains the Node
  implementation as fallback. The catalog exporter includes search/fetch-only
  providers so their virtual model metadata is available in Rust.
- Rust implements `/api/providers/suggested-models` with the existing model
  filters and a DNS-pinned public-URL fetch guard, and
  `/api/providers/kilo/free-models` with the one-hour/stale-cache behavior.
  Their Next.js routes retain the original implementations as fallbacks.
- Rust implements authenticated `/api/providers/client`, preserving usage
  eligibility, Codex JWT-claim backfill, safe-field redaction, sorting,
  filtering, pagination, and summary counts. The route proxies to Rust when
  `RUST_BE=true`; registry usage flags and original provider order are exported
  with the static catalog.
- Dynamic model-fetcher results for no-auth providers are still omitted from
  Rust `/api/models` and `/api/v1/models`; the Node implementation continues
  to include them when Rust routing is disabled.
- Chat usage cost uses the shared built-in rates and custom pricing stored in
  SQLite `kv` scope `pricing`. Regenerate the embedded rates after changes with
  `node backend-rust/scripts/sync-pricing.mjs`.
- Provider credential fields are removed from provider CRUD responses.
- Provider catalog export now includes aliases, category, display name, auth
  modes, and service kinds; `backend-rust/models.json` was regenerated.
- `backend-rust/README.md` and `.env.example` describe the single `RUST_BE`
  switch and the
  shared stable JWT secret requirement.

Relevant implementation files:

- `backend-rust/src/main.rs`
- `backend-rust/scripts/export-model-catalog.mjs`
- `backend-rust/models.json`
- `src/lib/rustBackendProxy.js`
- `src/app/api/providers/route.js`
- `src/app/api/providers/[id]/route.js`
- `src/app/api/keys/route.js`
- `src/app/api/keys/[id]/route.js`
- `src/app/api/provider-nodes/route.js`
- `src/app/api/provider-nodes/[id]/route.js`
- `src/app/api/combos/route.js`
- `src/app/api/combos/[id]/route.js`
- `next.config.mjs`
- `.env.example`

## Validation recorded 2026-10-08

- `cargo test --manifest-path backend-rust/Cargo.toml`: 38 tests passed,
  including model-catalog response and deduplication tests.
- `cd tests && npx vitest run unit/rust-backend-proxy.test.js`: 22 tests
  passed.
- ESLint passed for the changed model routes, proxy, and focused test file.
- `cargo fmt --manifest-path backend-rust/Cargo.toml --check` and
  `git diff --check` passed.
- Earlier API-key and provider-node HTTP smoke tests remain recorded below;
  they were not rerun for this model-list change.
- Earlier checks also included Clippy with warnings denied and `cargo build`;
  those were not rerun for this model-list change.
- Staging deployment and real cross-service shared-storage behavior have not
  been verified.

## Important unfinished work

### Provider and auth CRUD are intentionally partial

- Rust provider connection creation supports catalog providers with API-key
  auth (or `noAuth`) and configured custom nodes. OAuth-only providers, OAuth
  imports/refresh, bulk import, and provider-specific connection setup are
  still Node.
- The provider catalog is a model catalog, not a complete replacement for the
  Node `AI_PROVIDERS` definitions. Review/fill display metadata and provider
  aliases needed by the UI.
- Compare Rust behavior with
  `src/app/api/providers/route.js` and `src/app/api/providers/[id]/route.js`,
  especially validation, defaults, node-derived connection data, proxy/pool
  settings, priority handling, and update/activation side effects.
- Rust accepts catalog aliases/display names and configured provider nodes;
  compare remaining identifier normalization edge cases with Node.
- Rust `/api/models` lists generated static LLM models but does not include
  dynamic model-fetcher results for no-auth providers. The Gemini endpoint
  uses generated all-kinds entries, but its catalog response should still be
  compared against Node for complete registry and duplicate-name behavior.
- `/api/provider-nodes/validate` and provider-node-backed live model testing
  remain on Node.
- Provider OAuth credential flows remain on Node. Provider OIDC single
  sign-on now runs in Rust.
- Decide whether API keys with no `apiKey` are valid for each `noAuth` provider;
  currently empty credentials are stored in `data.apiKey`.

### Combo CRUD does not yet preserve all Node side effects

- Compare exact response/validation behavior against
  `src/app/api/combos/route.js` and `src/app/api/combos/[id]/route.js`.
- Dashboard combo writes proxied through Next reset the local Node rotation
  state and invalidate the allowed-model cache. Direct writes to Rust still
  bypass those process-local invalidations.
- `GET /api/combos/presets` and `POST /api/combos/presets` remain Node routes.

### Chat is not feature-parity

- Rust chat supports OpenAI and custom OpenAI-compatible chat nodes. Other
  catalog providers, translators, OAuth token refresh, multi-account
  fallback, and provider executor behavior remain Node.
- Combo routing supports fallback/round-robin among supported
  OpenAI-compatible members only. Fusion and non-compatible combo members are
  not implemented in Rust.
- Usage read APIs and token/cost aggregation are implemented, but dashboard
  stats/chart edge-case parity and pricing for unknown models are not fully
  verified. Unknown models without custom rates are recorded at zero cost.
- IP-whitelisted API keys are rejected by Rust until a trusted client-IP scheme
  is implemented. Do not trust arbitrary forwarding headers.
- The Next.js handler routes selectively; unsupported providers, key/IP cases,
  or model formats stay on Node. Verify this fallback against deployment
  topology before enabling `RUST_BE`.
- Add tests for error response contracts, stream/non-stream requests, upstream
  errors/headers, model aliases, configured base URLs, max-token field variants,
  ACL denial, RPM, and body limits.

### Proxy and production deployment

- `RUST_BE` must be set in the Next.js/Vercel build/runtime environment;
  deploy-specific settings are not verified. `NEXT_PUBLIC_RUST_API_URL` is
  still required to identify the Rust service.
- API-key generation requires Node-initialized `DATA_DIR/machine-id`; ensure
  `MACHINE_ID_SALT` and `API_KEY_SECRET` are identical wherever configured.
- Configure the same stable `JWT_SECRET` in Vercel and Rust. Do not allow
  Rust to generate or rotate a different JWT secret.
- Both Node and Rust must point to the same persistent database while sharing
  CRUD traffic. Vercel's local filesystem is not a shared persistent SQLite
  volume; settle the backend/database topology before enabling CRUD routing.
- `src/lib/rustBackendProxy.js` uses `NEXT_PUBLIC_RUST_API_URL` in server code.
  Confirm the configured URL is present at build/runtime, requests preserve
  cookie and CLI-token headers, and credentials never appear in proxy logs.
- Test Vercel rewrites/Next route handlers for collection paths, dynamic IDs,
  query strings, CORS preflight, and rollback with `RUST_BE=false`.
- Current deployment configuration, real shared storage, HTTPS, backup/restore,
  and production smoke tests have not been verified.

## Suggested next-task order

1. Validate the Rust OIDC implementation against real test IdPs and existing
   Node behavior; port remaining provider OAuth flows.
2. Finish missing provider-node validation/preset and provider-specific
   management endpoints, comparing every route with its Node contract.
3. Continue chat executor/translator parity and add missing behavior tests.
4. Verify one staging deployment with shared persistent DB, stable machine/API
   key secrets, and stable
   `JWT_SECRET`; verify behavior with `RUST_BE=false` before enabling the
   single `RUST_BE` switch and document rollback.

## Worktree caution

The repository already contains unrelated/in-progress dashboard, API-key-limit,
provider, and SSE edits. Preserve them. Do not revert broad worktree changes or
enable `RUST_BE` in production without explicit verification.
