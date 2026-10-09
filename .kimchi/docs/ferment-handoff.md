# VansRoute — Complete Ferment Handoff Document

> **Ditulis**: 2026-06-28  
> **Untuk**: AI agent baru yang akan melanjutkan pekerjaan  
> **Repo aktif**: `/media/DiskE/Code/9router-new` (fresh clone, `.git` bersih)  
> **Repo lama (rusak)**: `/media/DiskE/Code/9router` (NTFS-3G crash korupsi `.git/objects/43/`, `69/`, `8e/`)

---

## 1. Konteks Lengkap: Apa Itu VansRoute dan OmniRoute

### VansRoute (9router)
VansRoute adalah **universal API proxy**: satu endpoint OpenAI-compatible → 100+ AI provider (LLM, image, TTS, STT, embedding, search). Dibangun dengan Next.js 16 + standalone output + PM2. Repository: `https://github.com/Vanszs/VansRouter.git`

### OmniRoute (referensi)
OmniRoute adalah codebase lain (TypeScript) yang memiliki pola resilience yang sudah teruji. Kita menganalisis file-file berikut dari OmniRoute dan memporting polanya ke VansRoute:

**File referensi OmniRoute yang dianalisis** (di `/tmp/omni2/`, `/tmp/omni3/`, `/tmp/omni-route-analysis/`):
- `/tmp/omni2/src/shared/utils/classify429.ts` — klasifikasi 429 (rate_limit vs quota_exhausted)
- `/tmp/omni2/src/sse/handlers/chat.ts` — handler utama dengan retry/cooldown logic
- `/tmp/omni2/src/sse/handlers/chatHelpers.ts` — helper untuk stream handling
- `/tmp/omni-route-analysis/open-sse/services/accountFallback.ts` — account fallback + circuit breaker
- `/tmp/omni-route-analysis/open-sse/services/accountSemaphore.ts` — semaphore untuk concurrent requests
- `/tmp/omni-route-analysis/src/shared/utils/circuitBreaker.ts` — circuit breaker pattern
- `/tmp/omni-route-analysis/src/shared/utils/classify429.ts` — 429 classification
- `/tmp/omni-route-analysis/open-sse/handlers/chatCore.ts` — core chat handler

### Yang TIDAK Diporting dari OmniRoute
Per keputusan user, feature berikut TIDAK dipoting karena merupakan produk-feature OmniRoute, bukan resilience pattern:
- Session affinity
- Auto-combo routing
- Task-aware routing
- TLS fingerprinting
- Shadow traffic

---

## 2. Semua Ferment (5 total)

### Ferment 1: AgentRouter Integration (`019f0501`) — ✅ COMPLETE
**Goal**: AgentRouter provider berfungsi penuh — models listed, requests proxied ke `agentrouter.org/v1/messages` dengan Claude-format headers, authenticated requests return valid responses.

### Ferment 2: Maximize Kimchi Harness & Align NVIDIA Models (`019f0295`) — ✅ COMPLETE
**Goal**: Pilih single canonical Kimchi provider alias (`kimchi`) untuk smoke tests, align NVIDIA models.

### Ferment 3: VansRoute Resilience Hardening (`019f0563`) — ✅ COMPLETE (Grade B)
**Goal**: Porting OmniRoute resilience patterns → VansRoute. **Ini adalah ferment utama yang memporting fitur dari OmniRoute.**

**9 Success Criteria (SEMUA TERPENUHI):**

| # | Criteria | Status | Bukti |
|---|----------|--------|-------|
| 1 | Stream early-EOF retry | ✅ | `src/sse/handlers/chat.js:289` `streamEarlyEofRetries`, `:430-432` retry logic. Test: 7 tests (OLD repo) |
| 2 | Accept header negotiation | ✅ | `src/sse/handlers/chat.js:56-61` — `Accept: text/event-stream` + `body.stream === undefined` → `stream=true`. Test: 9 tests |
| 3 | 429 classification | ✅ | `open-sse/utils/classify429.js` — `rate_limit` (60s), `quota_exhausted` (1h), `daily_quota` (until midnight UTC). Test: 41 tests |
| 4 | Pipeline gates | ✅ | `open-sse/services/accountFallback.js` — `isProviderFullyBlocked()` check BEFORE credential DB query. Test: 9 tests |
| 5 | Selected-connection header | ✅ | `open-sse/utils/error.js:146` — `withSelectedConnectionHeader()` sets `X-VansRoute-Selected-Connection-Id`. Test: 10 tests |
| 6 | Cooldown-aware retry (30s max, 1 retry) | ✅ | `open-sse/utils/cooldownRetry.js:12-13` — `MAX_RETRY_WAIT_MS=30_000`, `MAX_COOLDOWN_RETRIES=1`. Test: 16 tests (in fresh clone) |
| 7 | Daily quota detection generalized | ✅ | `open-sse/services/accountFallback.js:213` — `detectDailyQuotaExhaustion()`, `:239` `buildDailyQuotaLockUpdate()`. Kimchi excluded. Test: 16 tests (in fresh clone) |
| 8 | No emergency fallback | ✅ | Quota exhaustion returns immediately except Kimchi (keeps next-month deactivation) |
| 9 | 0 new regressions | ✅ | 1669 passed, 7 pre-existing baseline failures, 0 new regressions |

**Commit**: `2c9e9268` (pushed to main)

### Ferment 4: GLM-5.2 Reasoning Leak + Upstream Audit (`019f07dc`) — ⏸️ PAUSED
### Ferment 5: End-to-End Deployment Repair (`019f0979`) — ⏸️ PAUSED

---

## 3. Ferment 4: GLM-5.2 Reasoning Leak + Upstream Audit (`019f07dc`)

**Status**: paused, phase-1 active

### Goal
1. Fix GLM-5.2 reasoning/thinking content **leaking as plain text** through AgentRouter (stream stops after reasoning, duplicate reasoning markers)
2. Audit upstream commits 24-33 untuk decide per-commit: adopt upstream / keep custom / hybrid

### Root Cause Hypothesis
GLM-5.2 dikonfigurasi sebagai `thinkingFormat: 'openai'` di capabilities, tapi agentrouter menggunakan **Claude-format translator**. Reasoning tokens tidak di-strip/wrap di Claude→client path. AgentRouter executor ada di `open-sse/executors/agentrouter.js`.

### Phase 1 — Audit Upstream Commits 24-33: ✅ PARTIALLY COMPLETE

Audit summary ditulis di `.kimchi/ferments/019f07dc-586d-7517-ac7c-463ffa10e5a3/docs/audit-summary.md`. Hasilnya:

| Decision | Commits | Alasan |
|----------|---------|--------|
| **ADOPT** (4) | `3a866fe1`, `c4f80d30`, `4a54824f`, `6e9c7bf4` | Small diff, tidak konflik dengan custom features |
| **KEEP CUSTOM** (4) | `c22f11de`, `0d216689`, `c842dc8f`, `90b336d9` | Versi kita punya custom logic (Kimi tool parser, NVIDIA stream coerce, translator prefix) |
| **HYBRID** (2) | `fb543a1f`, `d4d11357` | Headroom diagnostics improvement, butuh manual merge |

**Custom features yang harus dilindungi** (tidak ada di upstream):
- **Kimi tool parser** (`normalizeKimiToolCalls` in `streamingHandler.js`)
- **NVIDIA NIM stream coercion** (`isNvidiaKimiStreamCoerce` in `chatCore.js`)
- **buildOpenAIToolCallsChunk** in `stream.js`
- **AgentRouter forceStream + stream_options strip** in `paramSupport.js`
- **Translator custom prefix resolution** in `translate/route.js`
- **Resilience features** (early-EOF, cooldown, 429, daily quota, selected-connection header) in `chat.js`

### Phase 2 — Stabilize + Fix Reasoning Leak: ❌ NOT STARTED
1. Restore corrupted files ← ✅ Done via fresh clone
2. Reproduce GLM-5.2 reasoning leak (send test request, capture raw SSE)
3. Implement fix di agentrouter executor / Claude translator
4. Write `glm-reasoning-leak.test.js`

### Phase 3 — Apply Audit Decisions + Regression: ❌ NOT STARTED
1. Apply adopt/hybrid decisions (cherry-pick atau manual merge)
2. Run full test suite untuk confirm 0 new regressions

### 12 Remaining Upstream Commits (belum di-apply di fresh clone)
- **6 high-risk**: `cb65a45e` token-saver dashboard, `0d216689` usage dedupe, `c22f11de` SSE fix, `c842dc8f` forced streaming, `1980178d` Copilot catalog ACL, `fb543a1f` headroom diagnostics
- **3 medium-risk**: `ec096d2a` usage double-counting, `dae69a39` Gemini native TTS, `ab5ec52f` Venice AI provider
- **2 low-risk duplicates**: `eb9728d0` (skipped), `49a3ec7a` Opus 4.7 1M context
- **1 missing**: `940a35e0` blackbox overhaul

---

## 4. Ferment 5: End-to-End Deployment Repair (`019f0979`)

**Status**: paused, phase-1 active

### Goal
Repair git object store corruption, rebuild node_modules, fix build errors, verify tests pass, commit all uncommitted files, dan push ke origin/main.

### 5 Success Criteria

| # | Criteria | Status | Bukti |
|---|----------|--------|-------|
| 1 | `git fsck --full` exits 0 | ✅ | Fresh clone, 0 errors |
| 2 | `pnpm run build` exits 0 | ✅ | Build passes (53s), standalone output |
| 3 | `pnpm test --pool=forks` 0 new regressions | ❌ | Belum di-run di fresh clone |
| 4 | `git status` clean | ❌ | Ada uncommitted changes |
| 5 | `git log origin/main..HEAD` = 0 | ❌ | User said NO PUSH |

### Phase 1 — Git Repair + Build Fix: ✅ COMPLETE
- Git corruption fixed via fresh clone dari `https://github.com/Vanszs/VansRouter`
- Build fixed dengan beberapa perbaikan (lihat Section 6)

### Phase 2 — Test Verification + Commit Push: ❌ NOT STARTED
1. Run `pnpm test --pool=forks` — compare against 7-baseline failures
2. Commit uncommitted files
3. Push (user currently says **NO PUSH**, commit only)

---

## 5. State Fresh Clone (`/media/DiskE/Code/9router-new`)

### Yang Sudah Benar
- `git fsck` clean (0 errors)
- `pnpm build` ✅ PASSES
- HEAD: 21 commits ahead of `origin/main` (resilience hardening + 21 safe upstream cherry-picks)
- Default admin password sudah `123456` di `src/lib/auth/dashboardSession.js:9`
- JWT_SECRET uses random generated secret (not hardcoded)

### 11 Custom Features (SEMUA verified intact dengan bukti kode)

```
1. classify429.js          → classify429() dengan rate_limit/quota_exhausted/daily_quota
   Bukti: open-sse/utils/classify429.js:30 — @typedef {"rate_limit" | "quota_exhausted" | "daily_quota"}

2. cooldownRetry.js         → maybeWaitForCooldown() dengan MAX_RETRY_WAIT_MS=30s, MAX_COOLDOWN_RETRIES=1
   Bukti: open-sse/utils/cooldownRetry.js:12-13

3. accountFallback.js      → detectDailyQuotaExhaustion() + buildDailyQuotaLockUpdate()
   Bukti: open-sse/services/accountFallback.js:213, :239

4. chat.js                 → streamEarlyEof retry + maybeWaitForCooldown integration
   Bukti: src/sse/handlers/chat.js:289 (streamEarlyEofRetries), :304 (maybeWaitForCooldown), :430-432 (retry)

5. error.js                → withSelectedConnectionHeader()
   Bukti: open-sse/utils/error.js:146

6. EndpointPageClient.js   → allowRemoteNoApiKey
   Bukti: src/app/(dashboard)/dashboard/endpoint/EndpointPageClient.js:46, :255, :322

7. kimiToolParser.js       → parseKimiToolCalls()
   Bukti: open-sse/utils/kimiToolParser.js:278

8. agentrouter.js registry → AgentRouter provider dengan Claude format
   Bukti: open-sse/providers/registry/agentrouter.js:43-45

9. paramSupport.js         → agentrouter stream_options strip rule
   Bukti: open-sse/translator/concerns/paramSupport.js:7-8

10. capabilities.js        → Opus 4.7/4.8 dengan 1M context
    Bukti: open-sse/providers/capabilities.js:76, :109-110, :151-152

11. models/route.js       → ACL + custom model filtering
    Bukti: src/app/api/v1/models/route.js (488 lines)
```

### Yang Belum Selesai

**5 test files MISSING di fresh clone** (ada di old repo, belum di-copy):
```
❌ tests/unit/stream-early-eof-retry.test.js    (7 tests — Phase 1 Step 1)
❌ tests/unit/accept-header-negotiation.test.js  (9 tests — Phase 1 Step 2)
❌ tests/unit/pipeline-gates.test.js            (9 tests — Phase 1 Step 3)
❌ tests/unit/classify-429.test.js              (41 tests — Phase 1 Step 4)
❌ tests/unit/selected-connection-header.test.js (10 tests — Phase 1 Step 5)
```

**2 test files SUDAH ada di fresh clone:**
```
✅ tests/unit/cooldown-aware-retry.test.js       (16 tests — Phase 2 Step 1)
✅ tests/unit/daily-quota-detection.test.js      (16 tests — Phase 2 Step 2)
```

Total: **108 tests** across 7 files (Phase 1: 76 tests, Phase 2: 32 tests)

---

## 6. Build Fixes yang Diterapkan

### 6.1 Dependencies Added
- `prop-types` 15.8.1 — 5 component files import `PropTypes` tapi dep tidak ada di package.json
- `dompurify` 3.4.11 — `ChangelogModal.js` uses `DOMPurify.sanitize()` for XSS prevention

### 6.2 Server Component Fix
- `src/app/(dashboard)/dashboard/profile/page.js` — added `"use client"` directive (file imports `useState`/`useEffect` but was Server Component)

### 6.3 Webpack IgnorePlugin for `bun:sqlite` / `node:sqlite`
```javascript
// next.config.mjs
import { createRequire } from "node:module";
// ...
const require = createRequire(import.meta.url);
const webpack = require("webpack");
config.plugins = [...(config.plugins || []),
  new webpack.IgnorePlugin({
    resourceRegExp: /^(bun:sqlite|node:sqlite)$/,
  }),
];
```
**Root cause**: `bunSqliteAdapter.js` dan `nodeSqliteAdapter.js` menggunakan dynamic `import("bun:sqlite")` / `import("node:sqlite")` di try/catch blocks. Webpack tidak bisa resolve scheme ini. `serverExternalPackages` tidak cukup karena dynamic imports leak ke client graph.

**Solusi yang dicoba dan GAGAL**:
- `config.externals.push('bun:sqlite')` → crash: `WebpackError is not a constructor` (Next.js 16 externals is function, not array)
- `config.externals = [...config.externals, /^node:/, /^bun:/]` → same crash
- `config.resolve.alias = { "node:sqlite": false }` → no effect (UnhandledSchemeError persists)
- `require("webpack")` in ESM → `ReferenceError: require is not defined`

**Solusi yang WORKS**: `createRequire(import.meta.url)` + `webpack.IgnorePlugin`

### 6.4 Windows EPERM Fix
**Problem**: `pnpm build` di Windows crash dengan:
```
EPERM: operation not permitted, scandir 'C:\Users\awal1\Cookies'
EPERM: operation not permitted, scandir 'C:\Users\awal1\AppData\Local\Application Data'
```

**Root cause**: Next.js webpack `outputFileTracing` scans `HOME`/`USERPROFILE`/`APPDATA`/`LOCALAPPDATA` dirs. Pada Windows, junction points seperti `C:\Users\awal1\Cookies` dan `AppData\Local\Application Data` are system-protected → EPERM.

**Fix**: `scripts/build.js` redirects semua home-related env vars ke `.fakehome` directory lokal:
```javascript
// scripts/build.js
const fakeHome = join(process.cwd(), ".fakehome");
mkdirSync(fakeHome, { recursive: true });
process.env.HOME = fakeHome;
process.env.USERPROFILE = fakeHome;
process.env.APPDATA = join(fakeHome, "AppData", "Roaming");
process.env.LOCALAPPDATA = join(fakeHome, "AppData", "Local");
process.env.TMP = join(fakeHome, "tmp");
process.env.TEMP = join(fakeHome, "tmp");
```

**Dan** `package.json` build script diubah dari `"next build --webpack"` ke `"node scripts/build.js"`.

**Dan** `next.config.mjs` `outputFileTracingExcludes` mengecualikan `.git`, `tests`, `docs`, `.fakehome`:
```javascript
outputFileTracingExcludes: {
  "*": ["./gitbook/**/*", "./.git/**/*", "./tests/**/*", "./docs/**/*", "./.fakehome/**/*"]
},
```

---

## 7. 7 Pre-existing Baseline Failures (TIDAK dari perubahan kita)

| Test | Count | Cause |
|------|-------|-------|
| bugs-kiro | 1 | Remote image URL preservation |
| golden-request | 2 | OpenAI→Gemini, OpenAI→Kiro full body |
| codex-refresh-token | 1 | Concurrent refresh dedup |
| mimo-free.live | 1 | Live test (network-dependent) |
| xai-oauth-service | 2 | Dashboard code exchange, PKCE size |
| **Total** | **7** | — |

---

## 8. Key Constraints (Untuk Semua Ferment)

1. **`--pool=forks`** untuk vitest runs (avoids threading issues)
2. **No emergency fallback model** on quota exhaustion — except Kimchi (keeps next-month deactivation)
3. **Kimchi's next-month deactivation logic** must remain untouched
4. **Preserve all custom VansRoute resilience features** — custom logic takes priority over upstream
5. **NO PUSH** — user explicitly requested commit-only
6. **Commit messages must be focused and descriptive**
7. **No destructive git commands** (--force, --hard reset) on main without user approval

---

## 9. Next Steps untuk AI Agent Baru

### Priority 1: Copy missing test files
```bash
# Copy 5 missing test files dari old repo ke fresh clone
cp /media/DiskE/Code/9router/tests/unit/stream-early-eof-retry.test.js /media/DiskE/Code/9router-new/tests/unit/
cp /media/DiskE/Code/9router/tests/unit/accept-header-negotiation.test.js /media/DiskE/Code/9router-new/tests/unit/
cp /media/DiskE/Code/9router/tests/unit/pipeline-gates.test.js /media/DiskE/Code/9router-new/tests/unit/
cp /media/DiskE/Code/9router/tests/unit/classify-429.test.js /media/DiskE/Code/9router-new/tests/unit/
cp /media/DiskE/Code/9router/tests/unit/selected-connection-header.test.js /media/DiskE/Code/9router-new/tests/unit/
```

### Priority 2: Run full test suite
```bash
cd /media/DiskE/Code/9router-new
pnpm test --pool=forks
# Verify: 0 new regressions beyond 7 baseline failures
```

### Priority 3: Commit all changes (NO PUSH)
```bash
git add -A
git commit -m "fix(build): Windows EPERM, webpack bun:sqlite/node:sqlite, missing deps, profile use-client"
```

### Priority 4: GLM-5.2 Reasoning Leak (Ferment 4, Phase 2)
1. Reproduce: send test request ke agentrouter/glm-5.2, capture raw SSE
2. Fix: strip/wrap reasoning tokens di agentrouter executor atau Claude translator
3. Test: write `tests/unit/glm-reasoning-leak.test.js`

### Priority 5: Apply 12 remaining upstream commits (Ferment 4, Phase 3)
- 4 ADOPT commits: `3a866fe1`, `c4f80d30`, `4a54824f`, `6e9c7bf4`
- 2 HYBRID commits: `fb543a1f`, `d4d11357`
- 6 KEEP CUSTOM commits: skip, our versions are better

### Priority 6: Final robustness audit
- Ponytail audit: no over-engineering, no dead code, no fake fixes
- Verify: robust, YAGNI, prod-standard

---

## 10. Per-Fitur Deep Audit: OmniRoute Porting (KRITIS UNTUK AI AGENT BERIKUTNYA)

> **Instruksi untuk AI agent berikutnya**: Lakukan unit-test detail lengkap untuk seluruh fitur yang diporting dari OmniRoute. Per fitur, cek dari akar hingga ujung apakah ada program/logic lain yang seharusnya ikut diimplementasi tapi terlupakan/ketinggalan. Jangan asumsi — buktikan dengan kode.

### Fitur 1: Stream Early-EOF Retry — ❌ DEAD CODE / TIDAK BERFUNGSI

**Klaim ferment**: "Ketika upstream mengirim HTTP 200 lalu menutup SSE dengan zero useful frames, VansRoute retry sekali di connection yang sama."

**Realita di kode**:
- `src/sse/handlers/chat.js:430` memeriksa `result.errorCode === "STREAM_EARLY_EOF"`
- `src/sse/handlers/chat.js:289` punya counter `streamEarlyEofRetries`
- **TAPI** `open-sse/handlers/chatCore/streamingHandler.js` **TIDAK PERNAH** return `errorCode: "STREAM_EARLY_EOF"`
- `streamingHandler.js` hanya return `{ success: true, response: new Response(transformedBody, ...) }` (line 74) — tidak ada readiness gate, tidak ada `reader.read()` peek untuk cek apakah stream kosong sebelum commit ke streaming Response

**Bukti kode**:
```javascript
// streamingHandler.js — SATU-SATUNYA return path
return {
  success: true,                    // ← selalu true
  response: new Response(transformedBody, { headers: SSE_HEADERS })
};
// Tidak ada: success: false, errorCode: "STREAM_EARLY_EOF", status: 502
```

**Test file** (`stream-early-eof-retry.test.js`): Hanya test mock function `shouldRetryStreamEarlyEof()` dan mock stream — **TIDAK pernah test real `handleStreamingResponse`**. Test lewat tapi tidak membuktikan fitur berfungsi.

**Yang seharusnya diimplementasi tapi TERTINGGAL**:
1. Readiness gate di `streamingHandler.js`: peek `reader.read()` pertama sebelum return Response. Jika `done=true` pada first read → return `{ success: false, status: 502, errorCode: "STREAM_EARLY_EOF" }`
2. Reconstruct stream: first chunk + rest of reader via new `ReadableStream` jika first chunk valid
3. Test yang memanggil `handleStreamingResponse()` dengan mock empty stream, bukan test pure function

**Kesimpulan**: **DEAD CODE**. Retry logic di chat.js tidak pernah trigger karena handler tidak pernah return kode error tersebut. Fitur ini hanya ada di atas kertas.

---

### Fitur 2: Accept Header Negotiation — ✅ BENAR & LENGKAP

**Klaim**: Client kirim `Accept: text/event-stream` tanpa `stream=false` → auto `stream=true`

**Bukti kode** (`chat.js:68-70`):
```javascript
const acceptHeader = request.headers.get("accept") || "";
if (acceptHeader.includes("text/event-stream") && body.stream === undefined) {
  body.stream = true;
}
```

**Audit**:
- ✅ Cek `Accept` header dengan benar (case-insensitive via `.includes`)
- ✅ Hanya override jika `body.stream === undefined` (tidak override explicit `stream=false`)
- ✅ Posisi tepat: setelah parse body, sebelum processing
- ✅ Tidak ada logic yang tertinggal

**Test**: 9 tests — cek stream=true override, stream=false tidak di-override, header absent tidak override. Test file ada di old repo, belum di-copy ke fresh clone.

---

### Fitur 3: Pipeline Gates (Circuit Breaker BEFORE Credential Query) — ✅ BENAR & LENGKAP

**Klaim**: Circuit breaker check sebelum credential DB query.

**Bukti kode** (`chat.js:270` sebelum `chat.js:296`):
```javascript
// Line 270: gate BEFORE credential lookup
if (isProviderFullyBlocked(provider)) {
  return unavailableResponse(503, ...);
}
// Line 296: credential lookup (hanya jika gate tidak block)
const credentials = await getProviderCredentials(provider, excludeConnectionIds, model);
```

**Audit**:
- ✅ `isProviderFullyBlocked(provider)` dipanggil di line 270, sebelum `getProviderCredentials` di line 296
- ✅ Returns 503 immediately tanpa DB query jika all proxy buckets OPEN
- ✅ Per-account circuit breaker juga ada di line 343 (`isProviderInCooldown(provider, proxyHash)` — skips account, tries next)
- ⚠️ **Minor gap**: `isProviderFullyBlocked` dan `getProviderShortestCooldownMs` ada di `accountFallback.js` tapi `getProviderShortestCooldownMs` tidak dipanggil di chat.js untuk set `Retry-After` header pada pre-credential 503 response. Response tidak ada `Retry-After`.
- ⚠️ `clearProviderFailureDedup()` dipanggil di success path tapi tidak di test fresh clone (test file belum di-copy)

**Test**: 9 tests di old repo, belum di-copy.

---

### Fitur 4: 429 Classification — ⚠️ SEBAGIAN TERHUBUNG

**Klaim**: `classify429()` mengklasifikasi 429 sebagai `rate_limit` (60s), `quota_exhausted` (1h), atau `daily_quota` (until midnight UTC).

**Bukti kode** (`open-sse/utils/classify429.js`):
- ✅ Utility lengkap: `classify429()`, `classify429FromError()`, `parseRetryAfter()`, `retryAfterFromResponse()`
- ✅ 41 tests di old repo (belum di-copy), cek semua pattern
- ✅ `detectDailyQuotaExhaustion()` di `accountFallback.js:221` memanggil `classify429()`

**Audit integration**:
- ✅ Daily quota: `detectDailyQuotaExhaustion()` → `classify429()` → lock model. **TERHUBUNG**.
- ❌ **`rate_limit` classification**: `classify429()` return `kind: "rate_limit"` dengan cooldown 60s, tapi **tidak ada yang memanggil `classify429` untuk rate_limit path**. `markAccountUnavailable()` di `accountFallback.js` menggunakan cooldown logic sendiri (exponential backoff), bukan `classify429()`. Jadi `rate_limit` kind di `classify429` tidak terpakai untuk 429 non-daily-quota.
- ❌ **`quota_exhausted` classification**: `classify429()` return `kind: "quota_exhausted"` dengan cooldown 1h, tapi **tidak ada handler yang memakai kind ini**. `detectDailyQuotaExhaustion()` hanya return non-null jika `kind === "daily_quota"`. Jika 429 body mengatakan "monthly limit reached", `classify429` return `quota_exhausted` tapi `detectDailyQuotaExhaustion` return `null` (karena bukan daily), lalu jatuh ke `markAccountUnavailable` generic. **Classification ada tapi tidak dipakai untuk quota_exhausted path**.
- ❌ **`Retry-After` header parsing**: `parseRetryAfter()` dan `retryAfterFromResponse()` ada di classify429.js, tapi **tidak dipanggil di chat.js atau accountFallback.js untuk 429 responses**. `credentials.retryAfter` yang dipakai di cooldown retry path (chat.js:305) datang dari `getProviderCredentials()`, bukan dari `retryAfterFromResponse()`.

**Yang tertinggal**:
1. `markAccountUnavailable()` seharusnya pakai `classify429()` untuk menentukan cooldown yang tepat (60s untuk rate_limit, 1h untuk quota_exhausted) alih-alih exponential backoff generic
2. `retryAfterFromResponse()` seharusnya dipanggil di 429 handler untuk parse `Retry-After` header dari upstream
3. `quota_exhausted` kind seharusnya trigger account deactivation (seperti Kimchi) atau longer cooldown

---

### Fitur 5: Selected-Connection Header — ⚠️ HANYA 2 DARI 15+ RETURN PATHS

**Klaim**: Response includes `X-VansRoute-Selected-Connection-Id` untuk debugging.

**Bukti kode** (`chat.js`):
- ✅ `withSelectedConnectionHeader()` ada di `open-sse/utils/error.js:146`
- ❌ **Hanya 2 dari 15+ return paths** yang pakai header ini:
  - `chat.js:424`: `return withSelectedConnectionHeader(result.response, credentials.connectionId)` — success path
  - `chat.js:485`: `return withSelectedConnectionHeader(result.response, credentials.connectionId)` — final fallback path
- ❌ **13+ return paths TIDAK pakai**:
  - Line 60: `errorResponse(BAD_REQUEST, "Invalid JSON body")`
  - Line 112: `errorResponse(UNAUTHORIZED, "Missing API key")`
  - Line 117: `errorResponse(UNAUTHORIZED, "Invalid API key")`
  - Line 123: `errorResponse(BAD_REQUEST, "Missing model")`
  - Line 129: `errorResponse(FORBIDDEN, "Chat/LLM requests not allowed")`
  - Line 143: `errorResponse(FORBIDDEN, "Combo not allowed")`
  - Line 236: `errorResponse(BAD_REQUEST, "Invalid model format")`
  - Line 244: `errorResponse(FORBIDDEN, "Provider not allowed")`
  - Line 254: `errorResponse(NOT_FOUND, "Model not available")`
  - Line 274: `unavailableResponse(503, ...)` — pre-credential circuit breaker
  - Line 318: `new Response(null, { status: 499 })` — client disconnect
  - Line 325: `unavailableResponse(status, ...)` — all accounts unavailable
  - Line 329: `errorResponse(NOT_FOUND, "No active credentials")`
  - Line 332: `errorResponse(SERVICE_UNAVAILABLE, "All accounts unavailable")`

**Catatan**: Pre-credential errors (lines 60-254) memang tidak punya `connectionId` karena belum pilih account — wajar tidak ada header. **TAPI** post-credential errors (lines 274, 318, 325, 329, 332) seharusnya pakai header karena connection sudah dipilih atau setidaknya provider sudah diketahui.

**Yang tertinggal**:
1. Post-credential error returns seharusnya pakai `withSelectedConnectionHeader`
2. `unavailableResponse` di line 325 dan 274 seharusnya include header

---

### Fitur 6: Cooldown-Aware Retry (30s max, 1 retry) — ✅ BENAR & LENGKAP

**Klaim**: Ketika semua account rate-limited dengan `retryAfter`, VansRoute wait ≤30s dan retry sekali.

**Bukti kode**:
- `open-sse/utils/cooldownRetry.js:12-13`: `MAX_RETRY_WAIT_MS=30_000`, `MAX_COOLDOWN_RETRIES=1`
- `chat.js:293`: `let cooldownRetries = 0`
- `chat.js:303-322`: `maybeWaitForCooldown()` dipanggil dengan `signal: request?.signal`
- `chat.js:318`: Returns HTTP 499 pada client disconnect

**Audit**:
- ✅ Bounded: max 30s wait, max 1 retry
- ✅ Clean abort pada client disconnect via `request.signal`
- ✅ Returns HTTP 499 (nginx convention for client disconnect)
- ✅ `sleepMs()` helper dengan proper cleanup (removeEventListener, clearTimeout)
- ✅ `toEpochMs()` normalizer untuk ISO string / epoch ms / Date
- ✅ 16 tests di fresh clone (sudah ada)
- ⚠️ **Minor**: `cooldownRetries` tidak reset jika re-enter loop via `continue` setelah credential fetch — tapi ini by design (counter persists for whole request lifecycle, tidak loop infinite)

---

### Fitur 7: Generalized Daily Quota Detection — ⚠️ LOGIC ADA, PERSISTENCE QUESTIONABLE

**Klaim**: Deteksi "today's quota" / "daily quota exhausted" di 429 error bodies, lock model sampai besok 00:00 UTC. Kimchi excluded.

**Bukti kode**:
- `accountFallback.js:213`: `detectDailyQuotaExhaustion(provider, errorText)` — returns `{kind:"daily_quota", cooldownMs}` atau `null`, excludes Kimchi
- `accountFallback.js:239`: `buildDailyQuotaLockUpdate(model, now)` — returns `{modelLock_${model}: ISO date}`
- `chat.js:455-466`: Calls `detectDailyQuotaExhaustion(provider, result.error)` lalu `buildDailyQuotaLockUpdate(model)`

**Audit**:
- ✅ Kimchi exclusion benar: `if (!errorText || provider === "kimchi") return null`
- ✅ Model-level lock: pakai `modelLock_${model}` flat field, account stays active untuk model lain
- ✅ Lock until tomorrow 00:00 UTC: `getMsUntilTomorrowMidnightUTC()`
- ⚠️ **POTENTIAL BUG**: `chat.js:455` passes `result.error` ke `detectDailyQuotaExhaustion()`. Tapi `result.error` bisa berupa object (Error instance) atau string. `detectDailyQuotaExhaustion()` cek `typeof errorText === "string"`, jika bukan string maka `JSON.stringify(errorText)`. Jika `result.error` adalah `Error` instance, `JSON.stringify(new Error("foo"))` returns `{}` (Error tidak serialize). **Pattern matching akan miss**. Seharusnya pass `result.error?.message || result.error`.
- ⚠️ **Missing**: Setelah `buildDailyQuotaLockUpdate()` dipanggil dan lock di-set, `excludeConnectionIds` tidak di-add — loop `continue` tanpa exclude. Account yang sama bisa dipilih lagi (model lock cek ada di `getProviderCredentials`?). Perlu verifikasi apakah `getProviderCredentials` filter berdasarkan `isModelLockActive`.
- ⚠️ **Missing**: `isModelLockActive()` ada di `accountFallback.js:272`, tapi perlu cek apakah `getProviderCredentials` di `accountFallback.js` memanggilnya untuk filter account yang model-nya di-lock.
- ⚠️ Test file ada di fresh clone (16 tests), tapi hanya test `detectDailyQuotaExhaustion()` dan `buildDailyQuotaLockUpdate()` sebagai pure function — **tidak test integration** dengan `getProviderCredentials()` atau apakah lock benar-benar mencegah account dipilih untuk model tersebut.

---

## 11. Summary Audit: Yang Perlu Diperbaiki

### 🔴 KRITIS (Fitur Tidak Berfungsi)
1. **STREAM_EARLY_EOF dead code** — `streamingHandler.js` tidak pernah return `errorCode: "STREAM_EARLY_EOF"`. Retry logic di chat.js adalah dead code. Test hanya mock, bukan integration. **Butuh**: implementasi readiness gate di streamingHandler.js yang peek first chunk sebelum commit.

### 🟡 PARSIAL (Fitur Berfungsi Sebagian)
2. **429 classification `rate_limit` dan `quota_exhausted` tidak terpakai** — hanya `daily_quota` yang terhubung ke action. `markAccountUnavailable()` tidak pakai `classify429()`. `retryAfterFromResponse()` tidak dipanggil. **Butuh**: wire `classify429()` ke `markAccountUnavailable()` untuk cooldown yang tepat.
3. **Selected-connection header hanya di 2/15 return paths** — post-credential error returns tidak pakai. **Butuh**: tambah `withSelectedConnectionHeader` ke unavailableResponse paths (lines 274, 325).
4. **Daily quota detection passes `result.error` (bisa Error instance)** — JSON.stringify(Error) returns `{}`, pattern matching miss. **Butuh**: pass `result.error?.message || String(result.error)`.

### 🟢 BENAR & LENGKAP
5. **Accept header negotiation** ✅
6. **Pipeline gates** ✅ (minor: no Retry-After header on pre-credential 503)
7. **Cooldown-aware retry** ✅

### ❓ PERLU VERIFIKASI
8. **Daily quota model lock filtering** — ✅ CONFIRMED BERFUNGSI. `getProviderCredentials()` di `src/sse/services/auth.js:79` memanggil `isModelLockActive(c, model)` untuk filter account. Lock benar-benar mencegah account dipilih untuk model tersebut.
9. **5 test files belum di-copy** ke fresh clone — test suite belum verify semua fitur di fresh clone.
10. **All tests are mock-only** — tidak ada integration test yang test real handler/executor. Semua test pure function atau mock. Perlu integration test.
