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
| vLLM | _pending_ | |
| AgentX dataset | _pending_ | |
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
- **Legacy positional `BlockStored`:** `[tag, block_hashes, parent_block_hash, token_ids, block_size,
  lora_id?, medium?, lora_name?]`, i.e. **8 positions in the sim, not the 9 SPEC §6.2.4 states**. The
  field count is re-checked against vLLM under A3; the decoder must accept either.
- **Map format:** `--use-vllm-map-event-format true` (vLLM PR #42892 schema). In it,
  `parent_block_hash` is `null` instead of `0` when there's no cached prefix.
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

_Pending._

## A4 — vLLM flags

_Pending._

## A5 — vLLM `/v1/messages` with current Claude Code, cached-token usage

_Pending._

## A6 — vLLM metric names

_Pending._

## A7 — AgentX/Weka trace schemas, HF repo ids, license

_Pending._

## A8 — Session identifiers sent by Claude Code / OpenCode / Codex

_Pending._

## A9 — Upstream status

_Pending._

## A10 — Pure-Rust `zeromq` SUB against vLLM's PUB

_Pending._

## A11 — Small model for GPU validation

_Pending._
