# FINDINGS-M0 — recon results for SPEC Appendix A

Each item gives a **verdict**, the **evidence**, and **how it was verified**:
- **run**: observed on a running system;
- **source**: read in code or docs at the pinned version;
- **docs**: public documentation only.

Items verified by source reading only are labelled that way, and their runtime checks are listed in
SPEC §13 M5.

Date of checks: 2026-09-27.

## Pins

| Thing | Pin | Notes |
|---|---|---|
| Rust toolchain | 1.98.1 | `rust-toolchain.toml` |
| llm-d-inference-sim | tag `v0.11.2` = commit `7639395743c76f1cb37222981b33e8ea56cce69b` | latest release on 2026-09-27 (published 2026-08-31) |
| llm-d-inference-sim image | `ghcr.io/llm-d/llm-d-inference-sim:v0.11.2@sha256:32144df791330a0006b747edfdf2b114a0fe728e023a9d1b3463eeb48d32abb9` | |
| vLLM | `v0.30.0` = commit `ced6857afa0ea7b2e3f0846a62e1394e90f15607`; CPU image `vllm/vllm-openai-cpu:v0.30.0` | first release containing both PR #48048 and PR #51381 |
| AgentX dataset | `semianalysisai/cc-traces-weka-062126` @ `23f152f6f0f9399a85901b89a6458def0ef16729` | `traces.jsonl` sha256 `29b6a19e751ff5230771519aab755f80a0f43a4ba9cf96b72d3a6a437ec99276` |
| Claude Code | _pending_ | |

---

## A1 — Sim KV-cache mode without the render sidecar

**Verdict: ✅ works without the sidecar**, using the simulated tokenizer. **How: run + source.**

- The docs at v0.11.2 disagree with each other:
  - `docs/kv-cache.md` lists "the vLLM render sidecar must be running" as a prerequisite;
  - `docs/tokenization.md` says that with `--render-url` unset the simulator uses an in-process
    regex tokenizer with FNV-32a token hashes, with "no external service or network access".
- Running it settles it. With `--enable-kvcache true` and no `--render-url`, the log shows
  `"--render-url not set, using simulated tokenizer"` then `"Starting KV cache"`, and prefix caching works
  (A2).
- `POD_IP` must be set when KV cache is enabled (it goes into the ZMQ topic); the simulator refuses to
  start without it, per `kv-cache.md`.
- **Tokens per word:** a prompt of 2,000 words (`word0 … word1999`) came back as `prompt_tokens: 2005`.
  So the simulated tokenizer gives one token per whitespace-separated word, plus 5 tokens of chat-template
  overhead. This supports the spec's word mode (§6.4) and running the simulator at `--block-size 64`
  (allowed values: 8, 16, 32, 64, 128), so one trace `hash_id` equals one sim block.
- Default `max_model_len` is 1024; long-context runs need `--max-model-len`.
- **Consequence:** simulated token ids are pseudo-hashes and won't match real vLLM block hashes. That's
  fine: the gateway never needs to compute sim block hashes in v1.

## A2 — Sim reports `cached_tokens`; TTFT drops with hits under `per-token`

**Verdict: ✅ confirmed, and TTFT matches the documented formula.** **How: run.**

Setup:

```
llm-d-inference-sim:v0.11.2 --model test-sim-model --enable-kvcache true --block-size 64
  --kv-cache-size 256 --max-model-len 16384 --latency-calculator per-token
  --prefill-overhead 20ms --prefill-time-per-token 1ms --inter-token-latency 5ms --seed 1 --mode random
```

The same 2,005-token streaming chat request was sent three times (`stream_options.include_usage: true`):

| Run | `prompt_tokens` | `cached_tokens` | Predicted TTFT (`20 ms + uncached × 1 ms`) | Observed time-to-first-byte |
|---|---|---|---|---|
| 1 | 2005 | 0 | 2.025 s | 2.035 s |
| 2 | 2005 | 1984 | 0.041 s | 0.049 s |
| 3 | 2005 | 1984 | 0.041 s | 0.049 s |

- `1984 = 31 × 64`: only **full** blocks are cached; the trailing 21 tokens are always recomputed (same
  rule as vLLM).
- The usage shape is OpenAI's `usage.prompt_tokens_details.cached_tokens`.
- Metrics after the three runs:
  - `vllm:prefix_cache_hits_total = 3968` (2 × 1984);
  - `vllm:prefix_cache_queries_total = 6015` (3 × 2005);
  - `vllm:cache_config_info{block_size="64",num_gpu_blocks="256"}`.
- `docs/kv-cache.md` table: `per-token` and `default` (without `time-to-first-token`) both use
  `prefill-overhead + (prompt − cached) × prefill-time-per-token`. Setting `time-to-first-token` or using
  `constant` makes cache hits have **no** effect, so Tether's sim profiles must never set it.

### Extra sim findings that affect the design

- **ZMQ direction is reversed from vLLM.** The sim's `--zmq-endpoint` is the address the sim *dials*
  (`docs/kv-cache.md`: "the simulator dials this address"). A subscriber must **bind** its SUB socket and
  let the sim connect. vLLM's publisher binds instead (checked under A3/A10). The gateway's KV-event
  subscriber (§6.2.4) must support both, configured per worker.
- **Topic:** `kv@<POD_IP>:<port>@<model>`.
- **Frames:** `[topic, 8-byte big-endian seq, msgpack batch]`; batch = `[ts: f64, events, dp_rank?]`.
  This matches SPEC §6.2.4.
- **Legacy positional `BlockStored` has 9 positions on the wire**, as SPEC §6.2.4 says:
  `[tag, block_hashes, parent_block_hash, token_ids, block_size, lora_id, medium, lora_name, extra_keys]`.
  - `docs/kv-cache.md` lists only 8. The 9th, `extra_keys`, is in the source
    (`pkg/kvcache/kv_cache_sender.go`, `msgpackBlockStoredEvent`).
  - Array encoding can't omit a position, so empty optional fields arrive as `nil`.
  - Observed (A10): `["BlockStored", [15 hashes], 0, [960 token ids], 64, nil, "GPU", nil, nil]`.
- **Map format:** `--use-vllm-map-event-format true` (vLLM PR #42892 schema). Observed keys:
  `type, block_hashes, parent_block_hash, token_ids, block_size, medium`.
  - The source also defines `lora_id`, `lora_name`, `extra_keys`, `group_idx`, `kv_cache_spec_kind` and
    `kv_cache_spec_sliding_window`, all omitted when empty.
  - `parent_block_hash` is `null` instead of `0` when there's no cached prefix.
  - **No session field** in either format at v0.11.2.
- The sim imports its event types from `github.com/llm-d/llm-d-router/pkg/kvevents`. That is llm-d's
  router, so this is the format a production llm-d router consumes.
- **Batching:** events are flushed every `event-batch-size` events (default 16) or every 1 s.
- **Replay:** `--kv-events-replay-endpoint` binds a ROUTER. A request is `[identity, "", startSeq(8B BE)]`;
  replies are live-shaped frames, then a sentinel (`seq = 0xFFFF…FF`, empty topic and payload).
- **No session support** in the sim at v0.11.2: the docs have no `session_id` / `X-Session-ID` handling.
  This is what the §6.5 patch adds.
- **Eviction:** oldest unused block first (blocks of unloaded LoRA adapters go first). Blocks in use by a
  running request are never evicted.
- **Tool calls:** the sim can emit tool calls (`tool-call-*` probability flags in its config dump). It's
  useful for protocol tests, but the tool calls are random, not a real agent loop.

## A3 — vLLM `session_id` / `X-Session-ID` and session labels in `BlockStored`

**Verdict: ✅ confirmed at runtime and in source (v0.30.0).**

Runtime setup (used for A3–A6 and A10): `vllm/vllm-openai-cpu:v0.30.0` (`sha256:85d12674…`) serving
`Qwen/Qwen2.5-0.5B-Instruct` @ `7ae557604adf67be50417f59c2c2f167def9a775` (Apache-2.0; `model.safetensors`
sha256 verified), `VLLM_CPU_KVCACHE_SPACE=1`, `--kv-events-config
'{"enable_kv_cache_events":true,"publisher":"zmq","endpoint":"tcp://127.0.0.1:5560","topic":"kv"}'`.
A Rust `zeromq` SUB socket was bound on :5560 (see A10).

Runtime observations:

- A `BlockStored` event carried `session_id` for all three routes: the `X-Session-ID` header on
  `/v1/chat/completions`, the body `session_id` on `/v1/chat/completions`, and the `X-Session-ID` header on
  `/v1/messages`.
- Requests without a session id produced `BlockStored` events **without** the key (`omit_defaults`).
- Requests that were full prefix hits (`created_cache_tokens: 0`) produced **no** `BlockStored`, even
  with a session id. With the default report mode, reused blocks aren't re-announced, so the gateway
  can't learn "session S reused these blocks" from events alone.
- Observed map keys: `type, block_hashes, parent_block_hash, token_ids, block_size, lora_id, medium,
  lora_name, extra_keys, group_idx, kv_cache_spec_kind` (+ `session_id` when set).
- On the CPU backend, `block_size` is **128** and `medium` is still `"GPU"`. GPU vLLM defaults to 16.

Source:

- **Session id in:** PR #48048 (in v0.29.0+) adds `session_id` via:
  - the request body `session_id` on chat, completions and responses requests (`session_id: str | None`
    in `chat_completion/protocol.py` and `completion/protocol.py` at v0.30.0);
  - the HTTP header `X-Session-ID`;
  - a temporary fallback, `vllm_xargs["session_id"]`.

  It "does not add session-aware scheduling or routing"; it only carries identity inside the engine.
- **Session label out:** PR #51381 (in v0.30.0, **not** v0.29.0) adds `session_id: str | None = None` to
  `BlockStored` (`vllm/distributed/kv_events.py`). Details:
  - It is set for newly admitted full blocks, for reused prefix blocks (when `kv_cache_report_mode="full"`)
    and for partial prefix-cache entries.
  - It names the request that *triggered* the store; it does **not** mean exclusive ownership, since a
    block may be shared.
  - **`BlockRemoved` stays keyed only by hash, with no session.** To tell which session lost blocks,
    Tether must map hashes back to sessions itself.
- **Encoding at v0.30.0 is map-only.** `KVCacheEvent` is a `msgspec` struct with `tag=True` (key
  `"type"`) and `omit_defaults=True`, with no `array_like`. So `session_id` appears only when set, and
  vLLM no longer sends the legacy positional arrays; only the sim does, by default.
  - `BlockStored` map fields: `block_hashes, parent_block_hash, token_ids, block_size, lora_id, medium,
    lora_name, extra_keys, group_idx, kv_cache_spec_kind, kv_cache_spec_sliding_window, locality,
    ownership, session_id`.
  - `BlockRemoved` map fields: `block_hashes, medium, group_idx, locality, ownership`.
  - The batch itself (`EventBatch`) is still array-like: `[ts, events, data_parallel_rank?]`.
- **Publisher socket direction.** `ZmqEventPublisher` **binds** if the endpoint contains `*` (default
  `tcp://*:5557`), `::`, or is `ipc://` / `inproc://`; otherwise it **connects**. So Tether can bind one
  SUB socket per worker and have both vLLM (endpoint without `*`) and the sim dial in.
  - Replay uses a ROUTER socket, and each reply is `[client_id, "", topic, seq(8B BE), payload]`.
- `KVEventsConfig` fields: `enable_kv_cache_events, publisher ("null"|"zmq"), endpoint, replay_endpoint,
  buffer_steps, hwm, max_queue_size, topic` (default topic `""`).
- The sim (§6.5 patch) should emit `session_id` in the same map shape.

## A4 — vLLM flags

**Verdict: ✅ all present at v0.30.0 (source); partly confirmed at runtime.**

| Item | Where (v0.30.0) | Runtime |
|---|---|---|
| `--kv-events-config` (JSON) | `engine/arg_utils.py` | ✅ accepted by `vllm/vllm-openai-cpu:v0.30.0` |
| `--enable-prompt-tokens-details` | server args | ✅ accepted |
| `--num-gpu-blocks-override` | `engine/arg_utils.py` (`num_gpu_blocks_override`) | **source only; runtime check deferred to M5** (GPU-specific; see note) |
| `--block-size`, `--enable-prefix-caching`, `--max-num-seqs`, `--kv-cache-memory-bytes`, `--gpu-memory-utilization` | `engine/arg_utils.py` | — |
| `--enable-auto-tool-choice --tool-call-parser hermes` | server args | ✅ accepted |
| `ignore_eos` | request field on chat and completions (`ignore_eos: bool = False`) | ✅ exactly `max_tokens` generated, `finish_reason: "length"` |
| token-id prompts on `/v1/completions` | `completion/protocol.py` `prompt` field | ✅ `prompt: [1000..1299]` → `prompt_tokens: 300` |
| per-request `priority`, `cache_salt` | request fields on chat and completions | — |

Note: on the CPU backend, KV size is set by `VLLM_CPU_KVCACHE_SPACE` (GB), so
`--num-gpu-blocks-override` can only be meaningfully checked on a GPU.

## A5 — vLLM `/v1/messages` with current Claude Code, cached-token usage

**Verdict: ✅ works at runtime (CPU image, v0.30.0) with one required setting.** **How: run.**

### Direct API checks

- `/v1/messages` and `/v1/messages?beta=true` are both accepted.
- Non-streaming usage is Anthropic-shaped: `{input_tokens, output_tokens, cache_creation_input_tokens,
  cache_read_input_tokens}`. `input_tokens` counts only uncached tokens. Sending the same request twice
  moved 256 tokens from `cache_creation` (run 1) to `cache_read` (run 2).
- **Streaming quirk:**
  - `message_start.usage` reports `input_tokens` as the **total** (320) with no cache fields;
  - the final `message_delta.usage` reports `input_tokens: 64` plus `cache_read_input_tokens: 256`;
  - so the gateway must read cache usage from the **final `message_delta`**, not from `message_start`.
- Tools: `stop_reason: "tool_use"` with a `tool_use` content block (Qwen2.5 + `--tool-call-parser
  hermes`). On `/v1/chat/completions`: `finish_reason: "tool_calls"`.
- OpenAI shape on chat and completions: `usage.prompt_tokens_details = {cached_tokens,
  created_cache_tokens, multimodal_tokens}`.
  - **`created_cache_tokens` is new** and not in the spec: tokens newly written to the cache. It is
    useful for the miss explainer.
  - It is `null` on token-id completions.

### Claude Code 2.1.205 → structure-only logging proxy → vLLM

- Env: `ANTHROPIC_BASE_URL`, `ANTHROPIC_API_KEY=<dummy>`, all model aliases set to `qwen`,
  `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`.
- ❌ **Without a cap it fails:** Claude Code sends `max_tokens: 32000`, and vLLM returns 400 because
  prompt + 32,000 > `max_model_len` (32,768).
  **Required:** `CLAUDE_CODE_MAX_OUTPUT_TOKENS` (1024 used), or a model with a much larger context.
- ✅ With the cap, `claude -p "Reply with the single word: ok"` returned `ok` twice.
  - Each request had 2 messages, 20 tools, `max_tokens: 1024`, and ~13,704 prompt tokens (Qwen tokenizer).
  - **Run 2's first attempt read 13,696 tokens from cache** (`cache_read_input_tokens`), and TTFT fell
    from minutes (CPU prefill) to 0.36 s. **The system + tools prefix is byte-stable across separate
    Claude Code sessions.**
- Each `claude -p` run sent a different `x-claude-code-session-id` (hashed values differ). That's
  expected, since they are separate sessions.
- In run 1 the client dropped a slow **streaming** attempt (the proxy saw a connection reset) and a
  **non-streaming** request for the same turn followed. The gateway must expect duplicate attempts of
  one turn (lineage: identical fingerprints, one aborted). This is inferred from the proxy log, not
  confirmed from Claude Code's side.
- ⚠️ **Security note:** both runs sent an `authorization` header, not `x-api-key`, despite
  `ANTHROPIC_API_KEY` being set. So the OAuth bearer token very likely took precedence and went to the
  local vLLM. It stayed on this machine: the proxy stores header names only, and vLLM doesn't log
  headers by default. Future local runs will also set `ANTHROPIC_AUTH_TOKEN` to a dummy value.

## A6 — vLLM metric names

**Verdict: ✅ confirmed at runtime and in source (v0.30.0).**

- Scraped from `/metrics` on the CPU image: `vllm:num_requests_running`, `vllm:num_requests_waiting`,
  `vllm:num_requests_waiting_by_reason{reason=capacity|deferred}`, `vllm:kv_cache_usage_perc`,
  `vllm:prefix_cache_queries_total`, `vllm:prefix_cache_hits_total`, `vllm:cache_config_info{block_size,
  cache_dtype, enable_prefix_caching, gpu_memory_utilization, …}`.
- Every series has labels `engine="0"` and `model_name`. The sim uses `model_name` only.
- **Not verified:** whether `vllm:cache_config_info` carries a `num_gpu_blocks` label. My scrape
  truncated that line at 250 characters before reaching it. Runtime check deferred to M5. The sim does
  expose `num_gpu_blocks`. Either way, the per-worker `capacity_blocks` override (§6.2.3) is the fallback.

- Declared in `vllm/v1/metrics/loggers.py`:
  - `vllm:num_requests_running`, `vllm:num_requests_waiting`;
  - `vllm:kv_cache_usage_perc` (not the older `gpu_cache_usage_perc`);
  - `vllm:prefix_cache_hits` and `vllm:prefix_cache_queries`. These are counters declared without
    `_total`; the Prometheus client adds `_total` when exposing them, and the sim already exposes the
    `_total` names;
  - `vllm:cache_config_info`.
- **Useful new metrics for Tether:**
  - `vllm:kv_block_idle_before_evict_seconds` measures how long a block sits unused before eviction. That
    is a direct measurement of the LRU horizon `H(w)` that §6.1.6 *estimates* from the `BlockStored` rate.
  - `vllm:kv_block_lifetime_seconds` and `vllm:kv_block_reuse_gap_seconds`.
  - `vllm:num_requests_waiting_by_reason` and `vllm:prompt_tokens_cached`.
  - `vllm:external_prefix_cache_hits` / `_queries` (offloading connectors).
- Metric names are mapped per worker `kind` (§6.2.3); the sim and vLLM agree on all the names Tether uses.

## A7 — AgentX/Weka trace schemas, HF repo ids, license

**Verdict: ⚠️ partly differs from the spec.**
- ✅ Repo ids, the license and the canonical 393-trace set are as the spec assumes.
- ❌ The canonical set has **no** `stop` / `input_kind` / `input_types` fields and **no** SPAWN/JOIN
  topology. Only the *oldest* set (042026) has `stop` / `input_types`. **Spec decision needed** (see
  "Open questions" in [progress/M0.md](progress/M0.md)).

**How:** run (downloaded and scanned the full canonical file) + docs (dataset cards).

### Datasets (all `semianalysisai/*` on Hugging Face, all ungated, all Apache-2.0)

| Dataset | Revision (commit) | `traces.jsonl` | Trace format | Traces | Subagents | Has `stop` / `input_types` |
|---|---|---|---|---|---|---|
| **`cc-traces-weka-062126`** (canonical AgentX) | `23f152f6f0f9399a85901b89a6458def0ef16729` | 1,847,151,435 B, sha256 `29b6a19e…ec99276` (verified locally) | v7 | 393 | yes (1,697 groups) | **no** |
| `cc-traces-weka-062126-256k` | `8fecd2fc56694469f758f0afbbb6335ad3043740` | 568,864,747 B | v7 | _not downloaded_ | yes | presumably no |
| `cc-traces-weka-with-subagents-051926` (schema example linked in §17) | `78b029aad10404c2ffef3329001588f3326a234b` | 1,076,644,069 B | v5 | 219 (card) | yes | no (card) |
| `cc-traces-weka-042026` | `ae1ab9b568b8b5896c561692c40a6703cf5f8525` | 657,123,440 B | pre-v5 | 739 (card) | **no** | **yes** (card) |

There are 23 `cc-traces*` datasets in total. Most are intermediate builds (051226 … 061526, `-256k`
variants).

### Canonical set, measured on the pinned file

- 393 traces, 56,798 main-agent turns (56,432 streaming `s` + 366 non-streaming `n`), 1,697 subagent
  groups, 42,029 subagent inner requests (all `n`). This matches the dataset card's `stats.txt` exactly.
- Top-level keys: `id, models, block_size (=64 for all), hash_id_scope (="local" for all), requests`.
- Main request keys: `t, type, model, in, out, hash_ids, api_time`; plus `ttft` (streaming only) and
  `think_time` (absent on the 393 first turns).
- Subagent group keys: `t, type="subagent", agent_id, subagent_type, duration_ms, total_tokens,
  tool_use_count (null), status (all "completed"), requests, models`.
- Inner request keys: `t, type, model, in, out, hash_ids, api_time, think_time`.
- `subagent_type` is **always `"Subagent"`**. The traces can't distinguish a fresh subagent from a fork
  (§6.1.2 `subagent_kind`).
- **Not present anywhere:** `stop`, `input_kind`, `input_types`, `output_types`, SPAWN/JOIN.

### Facts from the dataset cards (reported by SemiAnalysis, not measured by us)

- **Classifier calls are filtered out** of every v5+ set: requests with `max_tokens ≤ 64` and no tools,
  i.e. title generation, "SUGGESTION MODE", Security Monitor. So the traces contain essentially **no
  `Side` traffic** (§6.1.2). Side-call handling can't be evaluated from traces; it needs E7's live capture.
- `in` and `hash_ids` are measured in the proxy's tokenizer (`o200k_base`). The card says Anthropic
  typically reports ~60% of that count for the same content. For v7, `in` = hash-block count × 64 and
  overcounts for very large contexts; requests with `in` > 990,016 were dropped.
- `hash_id_scope: local`: block ids are only comparable **within** one trace.
- 042026 additionally carries per-trace `system_tokens` and `tool_tokens`, and per-request
  `output_types`.

Downloads live in the gitignored `data/` directory, fetched by the pinned revision above.

## A8 — Session identifiers sent by Claude Code / OpenCode / Codex

**Claude Code: partial (free probe done; real capture pending the owner's go-ahead).** OpenCode:
_pending_. Codex: skipped (the owner decided not to install Node).

### Free probe (no request reached Anthropic)

- **Claude Code 2.1.205**, OAuth (subscription) login, `ANTHROPIC_BASE_URL=http://127.0.0.1:9199`,
  pointed at a local listener that recorded only method, path, header **names**, auth scheme and top-level
  body keys, then answered 503.
- ✅ An OAuth login **does** use a custom `ANTHROPIC_BASE_URL`. It sends `Authorization: Bearer …` and no
  `x-api-key`. So a logging passthrough to `api.anthropic.com` is feasible for the real capture.
- The request is `POST /v1/messages?beta=true`, so **the path carries a query string**. The gateway must
  match `/v1/messages` regardless of query.
- Header names sent:
  - `accept, accept-encoding, anthropic-beta, anthropic-dangerous-direct-browser-access, anthropic-version,
    authorization, connection, content-length, content-type, host, user-agent, x-app,`
  - **`x-claude-code-session-id`**,
  - `x-stainless-arch, -lang, -os, -package-version, -retry-count, -runtime, -runtime-version, -timeout`.
- **`x-claude-code-session-id` is a harness-specific session header**, the §6.1.2 step 2 candidate. Still
  to confirm in the real capture: is it stable across turns, and what do subagents and `/compact` send?
- Top-level body keys: `context_management, max_tokens, messages, metadata, model, output_config, stream,
  system, thinking, tools`.
- On a 503 it retried with the same headers (`x-stainless-retry-count` increments). Six attempts were seen
  in 25 s.

## A9 — Upstream status

**Verdict: ⚠️ one change from the spec: vLLM PR #53423 is merged, not open.** **How: GitHub API, checked
2026-09-27.**

| Item | Spec says | Status on 2026-09-27 | In a vLLM release? |
|---|---|---|---|
| vLLM PR #48048 session id plumbing | merged | merged 2026-08-03 | yes: v0.29.0 and v0.30.0 |
| vLLM PR #51381 `session_id` on BlockStored | merged | merged 2026-09-04 | **v0.30.0** (not v0.29.0) |
| vLLM PR #42892 KV events map encoding | — | merged 2026-06-09 | yes |
| vLLM PR #53423 KV hints request envelope | **open** | **merged 2026-09-21** | **no**: only on main after v0.30.0 |
| vLLM RFC #37003 retention API | open | open (updated 2026-09-09) | — |
| vLLM PR #38514 retention API impl | open | open (updated 2026-09-20) | — |
| vLLM RFC #57103 programmable KV | design only | open (updated 2026-09-24) | — |
| vLLM RFC #48501 session-centric KV | — | open (updated 2026-09-23) | — |
| llm-d #2584 | proposal | open, 1 comment | — |
| AIBrix #2814 | proposal | open, 1 comment | — |
| agentic-api #364 | plan | open, 0 comments | — |

"In a release" was checked with the GitHub compare API: the merge commit is an ancestor of the tag.
**What #53423 actually does, and whether it changes Tether's design, is checked under A3.**

## A10 — Pure-Rust `zeromq` SUB against vLLM's PUB

**Verdict: ✅ works against both the sim and vLLM v0.30.0 (run).**

- **vLLM:** the SUB socket was bound on `tcp://127.0.0.1:5560`, and vLLM dialed in (endpoint without
  `*`). Six map-encoded batches were received and decoded with `rmpv`: topic `kv` as configured, `seq`
  starting at **0** (the sim starts at 1).
- **Late subscriber, no loss:** the first batch received (seq 0) was from a request sent *before* this
  subscriber process started. When the publisher dials, ZMQ queues messages until a peer connects (up to
  its high-water mark). The flip side is that a restarted gateway gets stale events. The index must
  tolerate re-delivered `BlockStored`, and use `ts` to discard old batches.

Sim run:

- Throwaway test program (not committed): `zeromq = "=0.6.0"` (features `tokio-runtime`,
  `tcp-transport`, no default features), `rmpv = "=1.3.1"`, `tokio = "=1.53.1"`.
- A `SubSocket` **bound** to `tcp://127.0.0.1:5557`, subscribed to `""`. The sim (v0.11.2,
  `--zmq-endpoint tcp://127.0.0.1:5557`) dialed in.
- Four chat requests of 1000, 1000, 1500 and 2000 words produced one 3-frame message:
  - topic `kv@127.0.0.1:8010@test-sim-model`, seq 1, batch `[ts, events]`;
  - three `BlockStored` events with 15, 8 and 8 blocks. The repeated 1000-word request stored nothing
    (fully cached), and the longer prompts only added their new tail blocks, parented on the previous
    last block;
  - both the legacy positional and the map encodings decode with `rmpv`.
- Not yet exercised: `BlockRemoved` (the test never filled the 40-block cache), the replay ROUTER
  endpoint, and reconnection.

## A11 — Small model for GPU validation

_Pending._
