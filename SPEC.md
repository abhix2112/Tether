# Tether — agent-aware KV-cache routing & retention for self-hosted LLM serving

> **Keep every agent session tethered to its KV cache.**
> A single-binary gateway that sits in front of vLLM / SGLang workers, works out what each coding-agent
> session is doing from ordinary API traffic, and uses that to route requests to the worker that already
> holds the session's cache and to stop that cache from being evicted while the agent waits on a tool.

| | |
|---|---|
| Spec version | v1 — 2026-09-27 |
| Owner | Abhi ([@abhix2112](https://github.com/abhix2112)) |
| Working name | **Tether** (rename freely; nothing depends on the name) |
| Sister project | [Cachet](https://github.com/abhix2112/cachet) — response-level semantic cache (different layer, see §2) |
| License | MIT OR Apache-2.0 (same as Cachet) |
| Primary language | Rust (gateway, core, simulator, replayer) · Go (small patch to the llm-d simulator) · Python (plots only) |

### Changelog

- 2026-09-27 — §6.2.1, §6.1.7: `?beta=true` path matching; `created_cache_tokens`; Anthropic cache usage from the final `message_delta`; compressed upstream handling.
- 2026-09-27 — §3.3, §9: vLLM status updated (#53423 merged, plumbing only; #51381 in v0.30.0); vLLM pinned to v0.30.0.
- 2026-09-27 — §6.2.4: gateway binds one SUB per worker, workers dial in; map encoding primary; seq/ts dedupe; index rebuilt on restart.
- 2026-09-27 — §11.3: side-call handling and side-class fairness are measured only in E7 (traces contain no side calls).
- 2026-09-27 — §3.1, E1, E3: Dynamo pause figure labelled as reported; E1 reports full gap CDFs before TTL tuning; E3 adds short-pause-only and `max_ttl` variants.
- 2026-09-27 — §6.1.4, §3.4 H2, E4: add a session-history predictor for sim/E4; the tool-keyed predictor is evaluated only on live data.
- 2026-09-27 — §6.3: loader tags gaps spanning a subagent group as `SubagentWait`.
- 2026-09-27 — §6.3, §6.4, §6.1.4, E2: traces have no tool/user labels; sim treats main-turn gaps as tool pauses, 042026 supplies the prior and a 13% re-label sensitivity run.
- 2026-09-27 — §6.4: sim runs at `--block-size 64` in word mode (one hash_id = one sim block); real vLLM keeps its default.
- 2026-09-27 — §6.4, §17: canonical AgentX corpus `cc-traces-weka-062126` pinned as the reference; 051926 kept as a schema example.
- 2026-09-27 — §15: rules for captured traffic (outside the repo, redacted, structure-only commits).
- 2026-09-27 — §13 M0/M5: source-reading answers allowed in M0 if labelled; deferred runtime checks become M5 items.
- 2026-09-27 — §0.2: third-party numbers allowed only with an inline link and a "reported by <source>" label.

---

## 0. Read this first — rules for the coding agent

This document is the single source of truth. Read it fully before writing code.

1. **Work milestone by milestone (§13).** Do not start the next milestone until the current one meets its
   acceptance criteria. End each milestone with a short note in `docs/progress/Mx.md` (what was built,
   what was verified, what changed from the spec).
2. **Never invent numbers.** Every number that appears in the README, `docs/RESULTS.md`, or a chart must
   come from a committed result file (`results/**/*.json`) produced by `tether-sim` or `tether-bench`, with
   the exact command line stored inside that file. If a result is bad or inconclusive, report it as such.
   Numbers published by others (e.g. the AgentX figures in §3.1) may be quoted only with an inline link
   and the label "reported by <source>", and are never presented as Tether's results.
3. **Verify before you build on it.** Items marked **[VERIFY]** are external facts that were true on
   2026-09-27 according to public docs/issues but may have changed. Check each in M0, record the result in
   `docs/FINDINGS-M0.md`, and update Appendix A if reality differs. Adapt the design; don't hack around it.
4. **Pin everything:** vLLM version, llm-d-inference-sim commit, dataset revision, model revision, Rust
   toolchain (`rust-toolchain.toml`).
5. **`tether-core` is pure.** No I/O, no wall clock (time is passed in as an argument), no unseeded
   randomness. The gateway and the simulator both call the same `tether-core` decision code, so what is
   simulated is what ships.
6. **Ask the human before** spending money (GPU rental), opening PRs/issues on other people's repos, or
   publishing anything.
7. **Secrets:** never log `Authorization`, `x-api-key`, or prompt/response bodies at `info` level or above.
   Debug-level body logging must be opt-in and truncated.
8. **Small commits, tests with every change.** `cargo fmt`, `cargo clippy -D warnings`, `cargo test` must
   pass on every commit (CI enforces it).
9. In M0, create `CLAUDE.md` and `AGENTS.md` at the repo root containing: "Read SPEC.md §0 before doing
   anything. Current milestone: see docs/progress/." (Claude Code reads `CLAUDE.md`; Codex reads `AGENTS.md`.)
10. `docs/LEARNING.md` is the owner's learning guide. Don't rewrite it. When a milestone introduces a
    concept it doesn't cover, add a short entry with a link, and point to the relevant section from that
    milestone's progress note.

---

## 1. Summary

Coding agents (Claude Code, Codex, OpenCode, OpenClaw, custom harnesses) now generate a large share of LLM
traffic. Their requests look nothing like chatbot traffic: long multi-turn sessions, a huge context that
grows a little each turn, tiny outputs, pauses while tools run, and subagents that share big prefixes.
Hosted APIs (Anthropic, OpenAI) handle this with internal prompt caching. Teams that self-host open models
on vLLM or SGLang mostly get round-robin or generic prefix-aware routing plus LRU eviction. Those don't know
that a session is merely paused for a tool call and will be back in a few seconds.

Every open proposal to fix this (llm-d, AIBrix, vLLM, NVIDIA Dynamo; see §3.3) assumes **the agent harness
will send lifecycle hints** (open / heartbeat / close, priority, "speculative prefill"). Today no mainstream
harness does.

**Tether fills that gap.** It is:

1. **A gateway** that infers the agent lifecycle (session identity, turn lineage, tool pauses, subagent
   spawn/join, context compaction) from unmodified OpenAI-/Anthropic-compatible traffic. It also accepts
   explicit hints when a harness sends them.
2. **A policy engine** that uses those signals to (a) route each turn to the worker holding its cache,
   (b) predict how long a tool pause will last, and (c) retain valuable KV blocks during the pause within a
   bounded budget.
3. **Pluggable actuators** that apply retention on today's stock engines (LRU "touch", speculative prefill)
   and on the engine retention APIs being proposed (implemented in a patched GPU-free simulator, and in the
   vLLM retention PR when available).
4. **A reproducible evaluation** on SemiAnalysis's public AgentX traces: a deterministic simulator for fast
   policy sweeps, a trace replayer for end-to-end runs, and a small real-GPU validation.
5. **A cache-miss explainer** that attributes every miss to a cause (misroute, evicted during pause, prefix
   divergence, cold start).

---

## 2. Relationship to Cachet — why this is a new repo

Cachet and Tether are **different layers** and serve **different users**. Keep them as separate repos and
link each from the other's README ("from the author of Cachet").

| | **Cachet** (existing) | **Tether** (this spec) |
|---|---|---|
| What is cached | Whole **responses** (text) | **KV cache** (attention state) inside the inference engine |
| Effect on a hit | Upstream call skipped entirely | Call still made, but prefill of the shared prefix is skipped |
| Match rule | Same or semantically similar prompt | Same token prefix (exact) |
| Best workload | Repeated/rephrased questions, evals, retries | Multi-turn agent sessions (every turn is new, ~96% of its prefix is old) |
| Upstream | Paid APIs (OpenAI, Anthropic) | Self-hosted engines (vLLM, SGLang) on your GPUs |
| User | App developer paying per token | Platform/infra team running inference |
| Correctness risk | Serving a stale/wrong answer | None to output (only performance changes) |

Why not put Tether inside Cachet: for agent traffic a response cache hits almost never, because every turn
carries new tool output. Serving cached responses to an agent would also be actively dangerous. Mixing the
two would blur both projects' pitches.

**What to reuse from Cachet** (copy with attribution, same license): the axum/reqwest streaming proxy
structure, hop-by-hop header handling, the SSE tee (stream to client while inspecting), request-size
guards, the error types, and the style of the `/__cachet/` dashboard. Cachet's `src/proxy.rs` (~970 lines)
is the reference implementation for §6.2.1.

---

## 3. Problem

### 3.1 What agent traffic looks like

From SemiAnalysis **AgentX** (public benchmark built from opt-in Claude Code traces) as reported by the vLLM
team (Sept 2026):

- median **43 turns** per session;
- median input **~142K tokens**, median output **~444 tokens** per request;
- prefix-cache hit rate **above 96%** (each turn re-sends the whole conversation plus a small new tail);
- **44%** of sessions spawn at least one subagent (median 4 subagent rollouts in those sessions).

Reported by NVIDIA Dynamo from its analysis of Claude Code sessions: tool-call pauses of **2–30 s**
between turns; a write-once/read-many cache pattern (11.7× cache reads vs writes in one 42-call session);
and reasoning (`<think>`) tokens are roughly 40% of generated tokens but are never reused.

**Measured by us (M0 recon, FINDINGS-M0 §A7; to be regenerated as a committed E1 result):** on the
labelled 042026 traces, gaps after a `tool_use` response have p50 ≈ 9 s but p90 ≈ 124 s, i.e. a much
longer tail than "2–30 s". The two figures measure different things. TTL defaults are **not** tuned
until E1 has reported the full gap distributions.

### 3.2 Why this breaks self-hosted serving

1. **Session-blind routing.** With N workers and round-robin routing, turn k+1 lands on the worker holding
   turn k's cache with probability ~1/N. Each miss re-prefills ~100K+ tokens.
2. **LRU evicts during tool pauses.** While the agent runs a 20 s test suite, other sessions allocate
   blocks and LRU evicts the paused session's prefix. When the tool returns, the whole context is recomputed.
3. **Subagent cold starts.** Sibling subagents of the same type share a system prompt and tool definitions
   (often thousands of tokens) with each other, and "fork"-style subagents inherit the parent's whole prefix.
   But they land on arbitrary workers and recompute that shared prefix. (In Claude Code, a regular subagent
   has its own system prompt and tools, so its prefix differs from the parent's; forks copy the parent's.
   See Claude Code's prompt-caching docs in §17.)
4. **Value-blind eviction.** Ephemeral KV (finished subagents, closed reasoning blocks, side calls such as
   title generation) competes equally with the valuable, reused session prefix.
5. **Silent prefix breaks.** If the chat template re-renders the previous assistant turn differently from
   what was generated (e.g. strips reasoning), or the harness edits earlier messages (compaction,
   timestamps), the cache misses even on the right worker. Nobody tells the operator why.

### 3.3 State of the art (as of 2026-09-27) and the gap

| Project | What exists | What's missing for us |
|---|---|---|
| Hosted APIs | Internal prompt caching; explicit `cache_control` (Anthropic) | Not available to self-hosters |
| **NVIDIA Dynamo** | KV-aware router, priority scheduling, `nvext.agent_hints` (`priority`, `osl`, `speculative_prefill`) | Needs the harness to send hints; TTL/pin retention is stated as future API work |
| **llm-d** | Prefix-cache-aware scheduling (EPP), GPU-free `llm-d-inference-sim` with KV events | RFC [#2584](https://github.com/llm-d/llm-d/issues/2584) (opened 2026-09-25) proposes session pin/heartbeat/close + context-weighted eviction — **no implementation yet** |
| **AIBrix** | Distributed KV cache, prefix-aware routing | RFC [#2814](https://github.com/vllm-project/aibrix/issues/2814) (same idea) — proposal only |
| **vLLM** | Session identity plumbing (PR #48048, merged, in v0.29.0+); `session_id` on map-encoded `BlockStored` (PR #51381, merged, first released in **v0.30.0**); KV hints request envelope (PR #53423, merged 2026-09-21, on main only, **plumbing only: no actions are defined or executed**). Verified in M0 (FINDINGS-M0 §A3, §A9) | Retention API RFC [#37003](https://github.com/vllm-project/vllm/issues/37003) / PR #38514 **open**; programmable-KV RFC [#57103](https://github.com/vllm-project/vllm/issues/57103) is design only. Tether watches #53423 but does not depend on it |
| **vLLM agentic-api** | Rust gateway owning Responses/Messages state | Issue [#364](https://github.com/vllm-project/agentic-api/issues/364): *plans* to expose tool-execution lifecycle to llm-d cache policy |

**The gap Tether fills:**
- (a) An **engine-agnostic** layer that **infers** lifecycle signals from unmodified harness traffic, so the
  proposed APIs have something to feed them.
- (b) **Published, reproducible numbers** on AgentX traces, including ablations and a memory-pressure sweep.
- (c) A way to **test retention semantics without GPUs** (patched simulator).
- (d) A **miss explainer**.

It is likely that parts of this land upstream in the next few months. Tether's value is being first with a
working implementation and public data, and contributing to those upstream efforts (§16).

### 3.4 Hypotheses (report results either way)

- **H1.** Under KV-memory pressure, session affinity + lifecycle-aware retention reduces recomputed prefill
  tokens and p90 TTFT of tool-resume turns versus a strong prefix-aware router with LRU. The benefit shrinks
  toward zero when KV capacity is ample.
- **H2.** Pause length after a tool call is predictable enough to set TTLs: the predicted p90 covers
  ≥ 85% of actual pauses on held-out sessions. In simulation H2 is tested with the **session-history**
  predictor (the traces have no tool names). The tool-keyed predictor is tested only on live data (E7).
- **H3.** Program affinity (routing sibling subagents and forks to the worker already holding their shared
  system/tool or parent prefix) reduces subagent first-turn prefill.
- **H4.** From unmodified Claude Code / OpenCode traffic, the gateway attributes turns to the correct
  session with ≥ 95% accuracy and detects tool pauses with ≥ 90% precision and recall.

---

## 4. Goals and non-goals

**Goals**
- G1. Drop-in gateway: agents change only their base URL; workers need at most standard flags.
- G2. Works with stock vLLM today (routing + touch + speculative prefill), and shows what engine retention
  APIs would add (sim-pin).
- G3. Deterministic simulator that replays the full AgentX corpus in under a minute on a laptop.
- G4. Honest, reproducible evaluation: baselines, ablations, pressure sweep, seeds, confidence intervals.
- G5. Clear observability: Prometheus metrics, a decision log, a miss-attribution report, a live dashboard.
- G6. Upstream contributions: a patch to `llm-d-inference-sim` and data posted on the relevant RFCs.

**Non-goals**
- Not a new inference engine; no CUDA kernels; no changes to model execution.
- Not a response cache (that's Cachet).
- Not a multi-tenant API gateway (no auth, quotas, billing). A `tenant` label is carried for future isolation.
- Not production-hardened HA. Single instance; state is in memory and rebuilt from traffic.
- No Responses API (`/v1/responses`, used by Codex) in v1 — stretch goal.
- No KV transfer between workers (NIXL/LMCache) in v1 — note it as future work.

---

## 5. Architecture overview

### 5.1 Online path (gateway)

```
 unmodified agent harnesses              Tether gateway (one Rust binary)                           inference workers
 Claude Code / OpenCode / custom   ┌──────────────────────────────────────────────────────────┐   ┌──────────────────────┐
 / tether-bench replayer           │ ingress: /v1/chat/completions  /v1/messages              │   │ vLLM | SGLang |      │
        │  HTTP + SSE              │          /v1/completions   (+ passthrough for others)    │   │ llm-d-inference-sim  │
        └────────────────────────▶ │     │                                                    │   │ (prefix cache, LRU)  │
                                   │     ▼                                                    │   └──────────▲───────────┘
                                   │ hint parser ─▶ session resolver ─▶ lifecycle tracker     │              │
                                   │                 (tether-core)        (state machine)     │              │
                                   │                       │                    │             │              │
                                   │                       ▼                    ▼             │   forward    │
                                   │                   router (scores) ─────────────────────── ┼──────────────┤
                                   │                                                          │              │
                                   │ response inspector (SSE tee): stop reason, tool names,   │◀─── stream ──┤
                                   │   usage.cached_tokens ──▶ lifecycle tracker, miss explainer│             │
                                   │                                                          │              │
                                   │ pause predictor ─▶ retention policy ─▶ actuators ─────────┼─ touch / spec-prefill
                                   │                                        (touch, pin,      │  requests, /v1/kv/retain
                                   │                                         spec-prefill)    │  (sim fork), retention
                                   │                                                          │  fields (vLLM PR)
                                   │ KV index  ◀── ZMQ KV events (BlockStored/Removed) ◀───────┼──────────────┤
                                   │ load view ◀── scrape /metrics (running, waiting, kv %) ◀──┼──────────────┘
                                   │ /__tether/{metrics, sessions, decisions, dashboard}      │
                                   └──────────────────────────────────────────────────────────┘
```

### 5.2 Offline path (evaluation)

```
 AgentX / Weka traces (HF, Apache-2.0)
        │
        ├──▶ tether-bench load  ──▶ workload stats (E1)  ──────────────────────────────┐
        │                                                                              │
        ├──▶ tether-sim   (uses tether-core policies; deterministic)  ──▶ results/*.json ──▶ analysis/ (plots) ──▶ docs/RESULTS.md
        │                                                                              │
        └──▶ tether-bench replay ──▶ Tether gateway ──▶ sim workers or real vLLM ──▶ results/*.json
```

### 5.3 Key design decisions (write each as an ADR in `docs/adr/`)

- **ADR-1: Policy/mechanism split.** Tether decides *what* to keep, for how long, and where to route.
  Engines/actuators decide *how*. This mirrors the principle in vLLM RFC #37003 ("orchestrator defines
  policy; engine executes it").
- **ADR-2: Shared pure core.** One `tether-core` crate is used by both the gateway and the simulator.
- **ADR-3: Inference first, hints override.** Explicit hints (headers / `nvext.agent_hints`) always win
  over inferred signals.
- **ADR-4: Budgeted retention.** Retention is capped per worker so paused sessions can't starve new work.
- **ADR-5: Trace-level simulation at 64-token blocks.** This matches the AgentX `hash_ids` granularity, so
  no tokenizer is needed in the simulator.

---

## 6. Components

### 6.1 `tether-core` (library, pure, no I/O)

#### 6.1.1 Types

```rust
pub type Millis = u64;               // time is always passed in; never read a clock in core
pub struct WorkerId(pub u16);
pub struct SessionId(pub u64);       // interned
pub struct ProgramId(pub u64);       // hash(model, system prompt, tool definitions)

pub enum TrafficClass { Main, Subagent, Side }   // Side = title/summary/quota calls etc.

pub enum Phase {
    Generating,                       // request in flight
    ToolPause { tools: ToolSig, since: Millis },
    SubagentWait { children: SmallVec<[SessionId; 4]>, since: Millis },
    UserWait { since: Millis },       // assistant ended its turn; waiting for a human
    Closed { reason: CloseReason },
}

pub struct Session {
    id: SessionId, program: ProgramId, class: TrafficClass,
    parent: Option<SessionId>, phase: Phase,
    worker: Option<WorkerId>,
    last_prompt_tokens: u32, last_output_tokens: u32,
    lineage: Lineage,                 // message fingerprints of the last request (+ response)
    retention: Option<RetentionGrant>,
    turns: u32, created_at: Millis, last_seen: Millis,
}
pub struct RetentionGrant { worker: WorkerId, until: Millis, blocks_est: u32, value_density: f64 }
```

#### 6.1.2 Session resolver and lineage

Resolution order (first match wins):

1. **Explicit hint:** `X-Tether-Session` or `X-Session-ID` header, or `session_id` in the body (vLLM's field
   name **[VERIFY]**). A parent can be declared with `X-Tether-Parent-Session`.
2. **Harness-specific identifiers.** Discover these in M0 by capturing real traffic from Claude Code,
   OpenCode and (optionally) Codex, e.g. Anthropic `metadata.user_id` or harness session headers
   **[VERIFY — do not assume; record what each harness actually sends]**.
3. **Lineage matching** (always runs, and is used to validate 1–2):
   - Compute a fingerprint per message: `xxh3(role ‖ normalized content ‖ tool_call ids/names)`.
     Normalization trims whitespace and canonicalizes JSON key order for tool arguments.
   - Request R *continues* session S if R's message fingerprints start with S's last request fingerprints,
     followed by the assistant message S produced (allowing a "re-rendered assistant" mismatch; see
     §6.1.7), followed by new messages (tool results or a user message).
   - Index sessions by `hash(program_id, fingerprint of first user message)` for O(1) candidate lookup.
     Resolve ties by most recent `last_seen`.
4. **Otherwise** create a new session. Classify it:
   - `Side` if its program is "light" (different/smaller model, or system prompt below a configurable token
     count, or a known side-call pattern learned in M0).
   - `Subagent` if some parent is in `ToolPause` whose pending tools include a subagent launcher
     (configurable names, e.g. `Task`, `Agent`, `spawn_agent`), and the new session arrives within a
     window (default 10 s). Do **not** require the child's prefix to match the parent's: regular Claude
     Code subagents use their own system prompt and tools, while forks inherit the parent's full prefix.
     Record which kind it is (`subagent_kind: fresh | fork`), because it decides which worker holds the
     reusable prefix (siblings' worker vs parent's worker).
   - Otherwise `Main`.

**Compaction / rewrite detection:** if an explicit session id matches but lineage does not extend (earlier
messages removed or replaced), record a `PrefixRewrite` event (cause: `compaction` if the message count
dropped sharply, else `edited_history`). Release the old retention grant and start a new lineage in the
same session.

#### 6.1.3 Lifecycle state machine

| From | Event | To | Notes |
|---|---|---|---|
| (none) | first request | `Generating` | create session |
| `Generating` | response ends with tool call(s) | `ToolPause{tools}` | OpenAI `finish_reason=="tool_calls"`; Anthropic `stop_reason=="tool_use"`; or hint `X-Tether-Next: tool` |
| `Generating` | tool call to a subagent launcher | `SubagentWait` | children attach as they arrive |
| `Generating` | response ends normally | `UserWait` | `stop` / `end_turn` / hint `X-Tether-Next: user` |
| `Generating` | client disconnect / error | previous pause phase or `UserWait` | never grant retention on an errored turn |
| `ToolPause`, `SubagentWait`, `UserWait` | next request of the session | `Generating` | record the observed gap → predictor |
| `SubagentWait` | parent resumes | `Generating` | children → `Closed{Joined}`, release their grants |
| any pause | grant expires with no resume | `Closed{Expired}` (idle) | the session can still resume later, just without retention |
| any | hint `X-Tether-Next: end` | `Closed{Explicit}` | |

Multiple tool calls in one response: the tool signature (`ToolSig`) is the sorted set of tool names, capped
at 8.

#### 6.1.4 Pause predictor

- **Input:** observed gaps `gap = next_request_arrival − previous_response_end`, keyed by
  `(program_id, ToolSig)`. Fallback keys: `(*, ToolSig)`, then `(*, *)`. Cap key cardinality (default
  4096; LRU the keys).
- **Estimator:** log-spaced histogram (e.g. 48 buckets from 10 ms to 30 min) with exponential decay
  (half-life configurable, default 200 observations). It gives quantiles and a CDF `P(gap ≤ t)`.
- **Prior:** a global histogram of tool-resume gaps computed from the labelled 042026 trace set (gaps
  after requests with `stop == "tool_use"`), committed under `priors/` with provenance. Blend it with the
  per-key estimate using weight `n / (n + k)`, `k = 20`.
- **Output:** `Prediction { q50, q90, cdf(t) }`.
- **Two predictors behind one trait** (`PausePredictor`):
  - **Tool-keyed** (above): used by the gateway on live traffic, where tool names are visible.
    **Evaluated only on live data (E7)**, because no trace carries tool names.
  - **Session-history**: predicts from *this session's* recent gaps (an EWMA of log-gap plus a small
    window of recent gaps for quantiles), blended with the global prior using the same `n / (n + k)`
    weight, where `n` is the number of gaps seen so far in the session. **This is the predictor used in
    simulation and in E4.** The gateway can also use it as a fallback when a key has no data.
- **Evaluation (E4):** session-history predictor; train the prior on 70% of sessions, test on 30%. Report
  p90 coverage, MAE of q50, and a calibration plot.

#### 6.1.5 Retention policy

At response end for session S on worker W (skip `Side` sessions and errored turns):

```
C        = last_prompt_tokens + last_output_tokens          // context that will be re-sent
blocks   = ceil(C / block_size(W))
if phase == UserWait and cfg.user_wait_policy == None: no grant
ttl      = clamp(pred.quantile(cfg.target_q) * cfg.safety, cfg.min_ttl, cfg.max_ttl)   // defaults q=0.9, safety=1.2, 1s..120s
p_resume = pred.cdf(ttl)
benefit  = C * p_resume * prefill_cost_per_token(W)                          // seconds of GPU saved
lambda   = shadow_price(kv_usage(W))                                         // 0 below low_water (0.7), rising linearly to lambda_max at 1.0
cost     = blocks * ttl * lambda
grant if benefit > cost AND retained_blocks(W) + blocks <= beta * capacity(W)   // beta default 0.5
if over budget: evict the grant with the lowest value_density = benefit / (blocks * remaining_ttl)
                if S's density is higher
```

- Subagent sessions: after `Closed{Joined}`, release immediately (their KV has near-zero reuse value).
- Grants are renewed if the pause outlives the TTL **and** `cdf` says resumption is still likely (bounded
  by `max_ttl`).
- All constants live in config. Defaults are starting points; tune them in the simulator (E3) and record
  the chosen values in an ADR.

#### 6.1.6 Router

Implement a `Router` trait with these implementations (all needed as baselines):

| Router | Behavior |
|---|---|
| `RoundRobin` | naive baseline |
| `LeastLoaded` | min `(running + waiting) / max_num_seqs` |
| `StickySession` | consistent hash of session id (like vLLM Router's consistent hashing) |
| `PrefixAware` | score = expected prefix hit − load; uses the KV index (exact residency in the sim; KV events on real workers); no lifecycle knowledge. This is the **strong baseline** (roughly what llm-d's prefix-cache scorer does) |
| `Tether` | `PrefixAware` + lifecycle-aware residency model + program affinity + hysteresis |

`Tether` scoring for request R of session S:

```
score(w) = a * expected_hit_frac(w) - b * load(w) - c * kv_pressure(w)

expected_hit_frac(w):
  if w == S.worker:
      if S.retention active on w:            C_prev / C_now
      else:                                  (C_prev / C_now) * exp(-pause_age / H(w))   // LRU survival
  else if program(S) recently served on w:   program_prefix_tokens / C_now * exp(-age / H(w))
  else:                                      0
  (when the KV index has exact residency for R's blocks, use the exact longest-prefix match instead)

H(w) = LRU horizon ≈ capacity_blocks(w) / block_allocation_rate(w)   // from BlockStored event rate
load(w) = (running + waiting) / max_num_seqs(w)
```

Move a session off its worker only if `score(best) − score(current) > δ` (hysteresis, default 0.15).
`Side` traffic always uses `LeastLoaded`.

#### 6.1.7 Miss explainer

For each completed request with a known previous turn:

- `expected_cached` = token length of the common prefix with the session's previous request (+ its
  output). In v1, approximate it from lineage (messages that match). In v2, use exact block hashes.
- `actual_cached` = `usage.prompt_tokens_details.cached_tokens` (OpenAI shape) or
  `cache_read_input_tokens` from the final `message_delta` (Anthropic shape; verified on vLLM v0.30.0,
  FINDINGS-M0 §A5). `created_cache_tokens` / `cache_creation_input_tokens` are recorded alongside. They
  show what the request wrote to the cache, which helps separate `cold` from `evicted_in_pause`.
- If `actual_cached ≥ 0.9 × expected_cached` → `hit`. Otherwise assign the first matching reason:
  1. `cold` — first turn of this program on any worker.
  2. `misroute` — a different worker held the prefix (per KV index / last worker) than the one chosen.
  3. `evicted_in_pause` — same worker, and the session's blocks were removed during the pause (KV events
     with session labels), or cached tokens dropped with no lineage change.
  4. `prefix_divergence` — lineage breaks at message k. Sub-causes:
     - `assistant_rerender` — the re-sent assistant message differs from what was generated, e.g.
       reasoning stripped;
     - `system_changed`;
     - `tools_changed`;
     - `compaction`;
     - `edited_history`.
  5. `unknown`.

Emit these as metrics and in the decision log. `tether doctor` (a CLI subcommand, §8.4) turns the log into
a readable report.

### 6.2 `tether-gateway` (binary)

#### 6.2.1 Ingress and proxy

- **Endpoints handled with inspection:**
  - `POST /v1/chat/completions` (OpenAI);
  - `POST /v1/messages` (Anthropic; vLLM serves it for Claude Code **[VERIFY version]**);
  - `POST /v1/completions` (used by the replayer).
- **Path matching ignores the query string:** Claude Code calls `POST /v1/messages?beta=true`. The query
  string is forwarded unchanged.
- **Everything else** under `/v1/*` is passed through to the chosen worker unchanged. For `GET
  /v1/models`, use any healthy worker.
- **Streaming:** forward SSE chunk-by-chunk with no added buffering. Tee the stream into an incremental
  parser that extracts:
  - stop reason and tool names:
    - OpenAI: `choices[].finish_reason`, `delta.tool_calls[].function.name`;
    - Anthropic: `content_block_start` with `type: tool_use`, `message_delta.stop_reason`;
  - the final `usage`:
    - OpenAI shape: `usage.prompt_tokens_details.{cached_tokens, created_cache_tokens}`.
      `created_cache_tokens` (vLLM v0.30.0) is the number of prompt tokens newly written to the cache;
    - Anthropic shape: read cache usage (`cache_read_input_tokens`, `cache_creation_input_tokens`,
      uncached `input_tokens`) from the **final `message_delta`**. On vLLM v0.30.0, `message_start`
      reports the *total* as `input_tokens` with no cache fields (FINDINGS-M0 §A5).
  - If the upstream response is compressed (`content-encoding`), the tee decompresses a copy for
    inspection and forwards the original bytes unchanged.
- **Usage injection:** for OpenAI streaming, if the client didn't set `stream_options.include_usage`, the
  gateway sets it upstream so it can read `cached_tokens`. It then **removes the extra usage-only chunk**
  before forwarding, so the client sees exactly what it asked for.
- **Guards and headers:** carry over Cachet's request-body cap, hop-by-hop header stripping, and pure-Rust
  TLS (rustls).
- **Response headers added:** `x-tether-worker`, `x-tether-session`, `x-tether-route` (router reason
  code), and on non-streaming responses `x-tether-cached-tokens`.

#### 6.2.2 Hint ingestion

| Hint | Meaning |
|---|---|
| `X-Tether-Session` / `X-Session-ID` / body `session_id` | session identity |
| `X-Tether-Parent-Session` | declares a subagent's parent |
| `X-Tether-Next: tool\|user\|end` | what happens after this response (sent by hint-aware harnesses and the replayer) |
| `X-Tether-Expected-Resume-Ms` | harness's own estimate of the pause |
| `X-Tether-Priority: <int>` | scheduling/retention priority (higher = more important) |
| body `nvext.agent_hints` (`priority`, `osl`, `speculative_prefill`) | Dynamo-compatible hints, accepted for interoperability |

Strip all `X-Tether-*` headers and `nvext` before forwarding, unless the worker is configured as
`kind: dynamo`. Pass `session_id` / `X-Session-ID` to vLLM workers that support it, so KV events carry
session labels **[VERIFY]**.

#### 6.2.3 Worker registry and load view

Workers are configured statically (YAML) with `name`, `url`, `kind` (`vllm` | `sglang` | `sim` |
`dynamo`), `metrics_url`, `kv_events_endpoint`, and optional `capacity_blocks` / `block_size` overrides.

Every 500 ms (configurable), scrape Prometheus `/metrics` for:
- `vllm:num_requests_running`
- `vllm:num_requests_waiting`
- `vllm:kv_cache_usage_perc`
- `vllm:prefix_cache_hits_total`
- `vllm:prefix_cache_queries_total`
- `vllm:cache_config_info` (gives `num_gpu_blocks` and `block_size`)

Metric names differ across versions/engines **[VERIFY]**; map them per `kind`.

Health: mark a worker unhealthy after 3 failed scrapes or 5xx bursts. Route around it and drop its
retention grants.

#### 6.2.4 KV event subscriber and index

- **Socket direction:** the gateway **binds** one ZMQ SUB socket per worker and each worker **dials in**.
  - vLLM: set `--kv-events-config` `endpoint` to the gateway's address *without* `*`; vLLM's publisher
    connects when there is no wildcard.
  - llm-d-inference-sim: always dials its `--zmq-endpoint`.
  - Per-worker sockets identify the worker without relying on topics. (FINDINGS-M0 §A3, §A10.)
- Frames: `[topic, 8-byte big-endian seq, msgpack batch]`. The batch is `[ts, events, dp_rank?]`.
  vLLM's `seq` starts at 0, the sim's at 1.
- Encodings:
  - **named-field maps** (vLLM PR #42892) are **primary**. vLLM v0.30.0 sends only maps, and the sim is
    run with `--use-vllm-map-event-format true`;
  - legacy positional arrays are a **fallback decoder** (`BlockStored` has 9 positions, `BlockRemoved` 3,
    `AllBlocksCleared`).
- **Stale and re-delivered batches.**
  - A dialing publisher queues batches while no subscriber is connected, so a (re)started gateway can
    receive old batches.
  - Track `(worker, seq)`: drop duplicates and out-of-order repeats, and log gaps (resync via the replay
    endpoint where available).
  - Use `ts` to discard batches older than the gateway's start time.
  - Treat `BlockStored` idempotently.
  - **The index is in-memory and is rebuilt from scratch on gateway restart;** it is never persisted.
- Index per worker:
  - block hash → `{parent_hash, token_count, stored_at, session?}`;
  - per-session block lists when events carry session labels;
  - `BlockStored` rate → LRU horizon `H(w)` (EWMA).
- Use a pure-Rust ZMQ crate (`zeromq`) if its SUB socket works against vLLM's publisher **[VERIFY in M0]**;
  otherwise the `zmq` binding.
- v2 (optional): exact longest-prefix matching for routing requires computing vLLM-identical block hashes
  in the gateway (tokenizer + chat template + hash algorithm + seed). Treat it as stretch. Study llm-d's KV
  cache indexer as a reference.

#### 6.2.5 Actuators (`trait RetentionActuator`)

| Actuator | Works on | Mechanism | Notes |
|---|---|---|---|
| `noop` | all | nothing (LRU baseline) | |
| `touch` | stock vLLM/SGLang/sim | While a session holds a grant and `pause_age ≥ 0.5·H(w)` and `kv_usage(w) ≥ touch_threshold` (0.8), send the session's last full context (last request + assistant response) to the same worker with `max_tokens: 1`, `temperature: 0`, low priority if the engine supports it, header `x-tether-internal: touch`. This refreshes LRU recency. Budget: ≤ `touch_rps` per worker. | A workaround: it costs scheduler slots and a decode step. Measure the overhead. Excluded from client metrics. |
| `spec_prefill` | stock engines | At response end with a tool call, send `last request messages + assistant message (as the client will re-send it)` with `max_tokens: 1`, so the template-rendered assistant turn is cached before the tool returns. | Fixes `assistant_rerender` misses; measured separately. Matches Dynamo's `speculative_prefill` idea. |
| `sim_pin` | patched llm-d-inference-sim | `POST /v1/kv/retain {session_id, ttl_ms, priority, mode: retain\|pin}` and `POST /v1/kv/release {session_id}` | Shows what an engine-side retention API buys. See §6.5. |
| `vllm_retention` | vLLM built from PR #38514 (stretch) | Attach `retention_directives` / `retention_scope` request fields | Only if that PR is usable; label results "experimental branch". |
| `sglang_priority` | SGLang (stretch) | per-request priority for priority-aware radix eviction **[VERIFY]** | |

Every actuator call is recorded in the decision log with its cost (tokens, slot-time).

#### 6.2.6 Observability

- `GET /__tether/metrics` — Prometheus exposition:
  - `tether_requests_total{worker,class,route}`
  - `tether_prompt_tokens_total{worker}`, `tether_cached_tokens_total{worker}`
  - `tether_ttft_seconds{class,resume}` (histogram; `resume` = `first|tool|user`)
  - `tether_sessions{phase}`
  - `tether_retained_blocks{worker}`
  - `tether_retention_grants_total{outcome}` (`used|expired|evicted|released`)
  - `tether_actuator_requests_total{actuator,worker,outcome}`
  - `tether_pause_prediction_abs_error_seconds` (histogram)
  - `tether_miss_total{reason}`
  - `tether_kv_events_total{worker,type}`
  - `tether_lru_horizon_seconds{worker}`
- `GET /__tether/sessions` — JSON snapshot of sessions (no prompt content).
- `GET /__tether/decisions?since=` — recent decision log entries (route, scores, grant, actuator, miss
  reason). Also appended to `decisions.jsonl` when configured.
- `GET /__tether/` — self-contained dashboard (no CDN), styled after Cachet's. It shows workers (KV usage,
  retained blocks, horizon), sessions by phase, a live request feed with route and miss reasons, and a
  "prefill tokens saved" counter.
- `GET /__tether/healthz`.
- Bind to `127.0.0.1` by default; the dashboard has no auth (same stance as Cachet).

### 6.3 `tether-sim` (binary, deterministic discrete-event simulator)

- **Workers:** `capacity_blocks`, `block_size = 64` (trace granularity), `max_num_seqs`, and a cost
  profile:
  - `prefill_time = overhead + uncached_tokens × t_prefill`;
  - `decode per-token time = t_decode × (1 + γ·(batch − 1))`;
  - optional chunked prefill later.
  Profiles are loaded from `profiles/*.yaml`; one of them must be calibrated from real-GPU measurements in
  M5.
- **KV cache:** blocks with refcounts; unreferenced blocks in an LRU list. Retention is two modes, both
  with TTL expiry:
  - **retain** (soft): retained blocks are evicted only after all unretained unreferenced blocks, lowest
    priority first;
  - **pin** (strict): never evicted before expiry; allocation fails/queues instead.
- **Scheduler:** FCFS with `max_num_seqs` running; a request is admitted when its uncached blocks can be
  allocated. Otherwise it waits (report queueing time separately).
- **Workload:** AgentX/Weka traces via `tether-bench`'s loader crate.
  - Closed-loop concurrency N: keep N sessions active, and start the next session when one finishes.
  - Inter-turn delay = trace `think_time` (optionally scaled or capped).
  - Subagent groups start at their recorded offsets. The parent's next turn waits for the group (no trace
    version found in M0 carries SPAWN/JOIN topology; the group's position and timestamps are all we have).
- **Gateway logic:** the simulator calls the same `tether-core` router/lifecycle/predictor/retention code
  as the gateway, feeding it simulated events ("hint mode").
- **Lifecycle labels in simulation.** The canonical corpus has **no** `stop` / `input_kind` fields
  (FINDINGS-M0 §A7), so the simulator can't know whether a gap was a tool pause or a user wait.
  - **Subagent gaps:** the loader tags a main-turn gap that spans a subagent group (the group starts after
    the previous main turn and the next main turn waits for it) as **`SubagentWait`**, not `ToolPause`.
    Those gaps measure the subagent's run time, not a tool, and are excluded from tool-pause statistics
    and priors.
  - **Default for all other gaps:** a tool pause (`X-Tether-Next: tool`). In the labelled 042026 set,
    87% of turns end in `tool_use`, and gap length does not separate tool from user pauses, so no
    heuristic does better.
  - **Sensitivity run** (part of E2): re-label a random 13% of main-turn gaps as user waits. Choose which
    gaps with probability proportional to `P(user | gap length)` estimated from 042026, scaled so that
    13% are re-labelled overall. Keep the recorded durations. Report how E2's conclusions change.
- **Output:**
  - `requests.jsonl` (session, turn, worker, prompt/cached tokens, queue time, TTFT, E2E, miss reason);
  - `summary.json` (all metrics in §11.3, config, seed, git SHA, command line).
- **Performance target:** full corpus (≈393 sessions) at concurrency 32 in < 60 s on a laptop. Intern
  hash ids to `u32`, use `FxHashMap`, and avoid per-block allocation.
- **Determinism:** same seed + same config ⇒ byte-identical `summary.json`. Test this in CI.

### 6.4 `tether-bench` (trace loader + replayer)

**Loader** (a library crate shared with the simulator):
- Downloads/reads Weka-format traces (JSONL, one trace per line). Known fields:
  - top level: `id`, `models`, `block_size` (64), `hash_id_scope`, `requests[]`;
  - request: `t`, `type` (`n` / `s` / `subagent`), `model`, `in`, `out`, `hash_ids`, `api_time`,
    `think_time`, `ttft`;
  - subagent groups: `agent_id`, `subagent_type`, `duration_ms`, nested `requests[]`;
  - only the oldest build (`cc-traces-weka-042026`, 739 traces) has `stop`, `input_types`, `output_types`
    and per-trace `system_tokens` / `tool_tokens`. The canonical v7 set has none of these, and no build has
    `input_kind` or SPAWN/JOIN topology (FINDINGS-M0 §A7).
- **Labelled companion corpus:** `semianalysisai/cc-traces-weka-042026` @
  `ae1ab9b568b8b5896c561692c40a6703cf5f8525` (Apache-2.0) is used for the predictor prior (§6.1.4), E4, and
  the lifecycle-label sensitivity run (§6.3). It is never used for E2/E3 headline results.
- Reference corpus: **`semianalysisai/cc-traces-weka-062126`** (the canonical AgentX set: 393 traces, trace
  format v7, Apache-2.0), pinned at revision `23f152f6f0f9399a85901b89a6458def0ef16729`. Other builds (e.g.
  `cc-traces-weka-with-subagents-051926`, format v5) are schema examples only. Revisions, hashes and
  schemas are in `docs/FINDINGS-M0.md` §A7.
- Filters: `--max-context <tokens>`, `--sessions <N>`, `--seed`.

**Synthesizer** (content from hash ids):
- **Token-ID mode** (real vLLM): map each `hash_id` → 64 token ids from a seeded PRNG over a safe vocab
  range (exclude special tokens; read vocab size from config). Send
  `POST /v1/completions {prompt: [ids], max_tokens: out, ignore_eos: true, temperature: 0, stream: true}`
  **[VERIFY token-id prompts and `ignore_eos` in the pinned vLLM]**.
- **Word mode** (sim with its simulated tokenizer): map each hash id → 64 words from a fixed word list. The
  sim's tokenizer gives one token per whitespace-separated word (verified in M0: 2,000 words → 2,005 chat
  tokens, the extra 5 being chat-template overhead). Run `llm-d-inference-sim` with **`--block-size 64`**,
  so one trace `hash_id` = one sim block. Send word-mode prompts to `/v1/completions` (no chat template)
  so block boundaries line up exactly; confirm the zero-overhead count in M1. Real vLLM keeps its own
  default block size (16 on GPU), and token-ID mode maps one `hash_id` to 64 tokens = 4 vLLM blocks.

**Replayer:**
- Closed-loop per session, concurrency N, trace timing with `--think-scale` and `--think-cap`.
- Subagents run concurrently with correct join behavior.
- Hint mode (on by default): sends `X-Session-ID`, `X-Tether-Parent-Session`, and `X-Tether-Next`
  (derived from whether the next entry is a tool result or user input).
- **Limitation to document:** the server's generated output tokens won't match the trace's recorded output
  blocks. So KV of decode tokens is never reused in replay — only input-prefix blocks. Outputs are tiny
  (median ~444 tokens), so the effect is small. The same limitation applies to other replay tools.
- **Records:** client-side TTFT, ITL, E2E per request; `cached_tokens` from usage; session completion time;
  total replay wall time; into `results/<run-id>/`.
- **Cross-check:** run the round-robin baseline with both `tether-bench` and NVIDIA AIPerf (its AgentX
  scenario) against the same workers. Report agreement. This establishes that our harness isn't biased.

### 6.5 Patch to `llm-d-inference-sim` (Go) — `patches/llm-d-inference-sim/`

The llm-d simulator already models vLLM's prefix cache (block-level, LRU of unused blocks), makes TTFT
depend on uncached tokens (`prefill-time-per-token`), reports `usage.prompt_tokens_details.cached_tokens`,
publishes ZMQ KV events, and serves `/v1/messages`. Add:

1. **Session labels:** accept `X-Session-ID` / `session_id`. Track session → blocks, and include the
   session label in `BlockStored` events in the same shape vLLM uses **[VERIFY vLLM PR #51381 field name]**.
2. **Retention:**
   - `POST /v1/kv/retain {session_id, ttl_ms, priority, mode}` and `POST /v1/kv/release {session_id}`;
   - an optional per-request `retention_directives` / `retention_scope` field mirroring vLLM RFC #37003;
   - eviction order changes as in §6.3 (retain vs pin);
   - new metrics `vllm:retained_blocks` and `tether_sim_retention_expired_total`.
3. Unit tests in the sim's style, and docs in its `docs/kv-cache.md` style.

Keep the patch small and self-contained, and maintain it in a fork. **Before opening an upstream PR, the
human will open an issue on llm-d-inference-sim** proposing "retention directive simulation for agentic
cache-policy research" and link llm-d RFC #2584. The upstream PR is a portfolio goal.

### 6.6 `analysis/` (Python, plots only)

- `uv`-managed project with polars/pandas + matplotlib.
- `make results` regenerates every chart in `docs/RESULTS.md` from `results/**/*.json`.
- No numbers are typed by hand.

---

## 7. Configuration (example `tether.yaml`)

```yaml
listen: 127.0.0.1:9100
upstream_timeout_s: 600
workers:
  - { name: w0, url: http://127.0.0.1:8000, kind: vllm, metrics_url: http://127.0.0.1:8000/metrics, kv_events: tcp://127.0.0.1:5557 }
  - { name: w1, url: http://127.0.0.1:8001, kind: vllm, metrics_url: http://127.0.0.1:8001/metrics, kv_events: tcp://127.0.0.1:5558 }
router:
  policy: tether            # round_robin | least_loaded | sticky | prefix_aware | tether
  weights: { hit: 1.0, load: 0.6, pressure: 0.3 }
  hysteresis: 0.15
lifecycle:
  subagent_tool_names: [Task, Agent, spawn_agent]
  side_call_max_system_tokens: 800
  subagent_window_ms: 10000
predictor:
  prior: priors/agentx-tool-gaps.json
  half_life_obs: 200
  prior_strength: 20
retention:
  enabled: true
  actuator: touch           # noop | touch | spec_prefill | sim_pin | vllm_retention
  target_quantile: 0.9
  safety: 1.2
  min_ttl_ms: 1000
  max_ttl_ms: 120000
  budget_fraction: 0.5
  low_water: 0.7
  user_wait_policy: none    # none | short (min_ttl)
touch:
  kv_usage_threshold: 0.8
  max_rps_per_worker: 5
observability:
  decisions_log: ./decisions.jsonl
  dashboard: true
```

Every field is overridable by env var `TETHER__SECTION__KEY` (figment or config-rs). Validate on startup
and fail with a clear error.

---

## 8. Interfaces

### 8.1 Client-facing
OpenAI and Anthropic compatible. Clients change only their base URL, e.g. `OPENAI_BASE_URL` or
`ANTHROPIC_BASE_URL=http://127.0.0.1:9100`.

### 8.2 Hints
See §6.2.2. Document them in `docs/HINTS.md` with a copy-paste example for a custom Python agent.

### 8.3 Admin
`/__tether/*` (§6.2.6).

### 8.4 CLI (`clap`)

```
tether serve   --config tether.yaml
tether doctor  --decisions decisions.jsonl [--html report.html]   # miss attribution report
tether-sim     run   --traces <path|hf-id> --policy tether --workers 4 --capacity-frac 0.5 --concurrency 32 --seed 1 --out results/...
tether-sim     sweep --grid sweeps/e2.yaml --out results/e2/
tether-bench   stats  --traces ...                     # E1 workload characterization
tether-bench   replay --target http://127.0.0.1:9100 --traces ... --sessions 60 --concurrency 16 --mode token-ids --hints on --out results/...
```

---

## 9. Tech stack

| Area | Choice | Why |
|---|---|---|
| Language | Rust (stable, pinned) | Same as Cachet; low overhead in the proxy path; shared core for sim and gateway |
| Async/HTTP | `tokio`, `axum 0.8`, `reqwest` (rustls, stream, http2) or `hyper-util` client | Proven in Cachet |
| Serialization | `serde`, `serde_json`, `rmp-serde`/`rmpv` (msgpack), `serde_yaml` | KV events are msgpack |
| ZMQ | `zeromq` (pure Rust) or `zmq` binding | Subscribe to vLLM/sim KV events |
| Hashing | `xxhash-rust` (xxh3) for fingerprints; `rustc-hash` maps | Fast, deterministic |
| Concurrency | `dashmap` or sharded `parking_lot` mutexes; `arc-swap` for config | |
| Metrics | `metrics` + `metrics-exporter-prometheus`; `hdrhistogram` in bench/sim | |
| Config/CLI | `figment` (or `config`), `clap` | |
| Logging | `tracing`, `tracing-subscriber` (JSON option) | |
| Testing | `proptest` (lineage, state machine), `insta` (snapshot of reports), `wiremock` or in-process fake workers, `criterion` (hot paths) | |
| Simulator patch | Go (llm-d-inference-sim's language) | Upstreamable |
| Analysis | Python 3.12 + `uv`, polars, matplotlib | Plots only |
| Local stack | Docker Compose: gateway + 2–4 patched sim workers + Prometheus + Grafana | Runs on a laptop, no GPU |
| GPU validation | vLLM **v0.30.0** (commit `ced6857afa0ea7b2e3f0846a62e1394e90f15607`; CPU image `vllm/vllm-openai-cpu:v0.30.0` for GPU-free checks), 1 GPU running 2 instances or 2 GPUs; small open instruct model with tool-calling support **[VERIFY a current good choice, ~4–8B]** | Cheap and real |
| CI | GitHub Actions: fmt, clippy, test, determinism check, 60-second sim smoke run on a synthetic fixture | |
| Stretch infra | kind + llm-d (EPP with prefix-cache scorer) as an extra baseline; K8s manifests | Credibility with infra reviewers |

---

## 10. Repository layout

```
tether/
├── Cargo.toml                    # workspace
├── rust-toolchain.toml
├── SPEC.md  CLAUDE.md  AGENTS.md  README.md  SECURITY.md  CONTRIBUTING.md  LICENSE-MIT  LICENSE-APACHE
├── crates/
│   ├── tether-core/              # pure: types, lineage, lifecycle, predictor, retention, router, explainer
│   ├── tether-gateway/           # axum server, proxy/{openai,anthropic,completions,sse}, upstream,
│   │                             # workers (registry, scraper), kv_events, index, actuators/*, metrics, dashboard/
│   ├── tether-traces/            # Weka/AgentX loader + synthesizers (shared by sim and bench)
│   ├── tether-sim/               # discrete-event simulator
│   └── tether-bench/             # stats + replayer
├── patches/llm-d-inference-sim/  # patch series + apply script + README
├── profiles/                     # simulator cost profiles (one calibrated in M5)
├── priors/                       # predictor priors derived from traces (generated, committed with provenance)
├── sweeps/                       # experiment grids (YAML)
├── deploy/
│   ├── compose/                  # docker-compose.yaml, prometheus.yml, grafana dashboards
│   └── gpu/                      # scripts to launch vLLM workers with pinned flags
├── analysis/                     # python plots → docs/img/
├── results/                      # committed JSON results (small); large raw logs gitignored
└── docs/
    ├── ARCHITECTURE.md  HINTS.md  BENCHMARK-PROTOCOL.md  RESULTS.md  FINDINGS-M0.md
    ├── adr/                      # ADR-1..N
    └── progress/                 # M0.md … M7.md
```

---

## 11. Evaluation protocol

### 11.1 Experiments

| ID | Question | Where | Setup |
|---|---|---|---|
| **E1** | Does our loader see the same workload as published? | loader | Turns/session, input/output lengths, prefix reuse, subagent share. **Full gap CDFs (p50/p75/p90/p99 and plot)** for main-turn gaps, `SubagentWait` gaps, and subagent inner gaps (062126), plus tool vs user gaps (042026). Reported before any TTL default is tuned. Must be close to published AgentX figures (§3.1); explain any differences. |
| **E2** | H1: does lifecycle-aware retention help, and when? | sim | All 5 routers × actuator ∈ {noop, retain, pin, touch-model} × KV capacity ∈ {0.25, 0.5, 0.75, 1.0, 1.5}× aggregate working set × concurrency ∈ {8, 16, 32, 64} × workers ∈ {2, 4, 8}; 3 seeds. Plus the lifecycle-label sensitivity run (§6.3) on a representative subset |
| **E3** | Which parts matter? | sim | Ablations: −predictor (fixed TTL 30 s), −program affinity, −budget, retain vs pin, `user_wait_policy`; **short-pause-only** retention (grant only when predicted q90 < `max_ttl`); `max_ttl` ∈ {30, 60, 120, 300} s |
| **E4** | H2: is pause length predictable? | loader + core | Session-history predictor (§6.1.4); 70/30 split by session; p90 coverage, q50 MAE, calibration curve. Tool-keyed predictor: live data only, in E7 |
| **E5** | Does the real gateway match the simulator? | compose (gateway + patched sim workers) | Same configs as a subset of E2; per-metric agreement within tolerance (target ±10%) |
| **E6** | Does it work on real GPUs? | 2 vLLM workers | `prefix_aware` vs `tether+touch` vs `tether+touch+spec_prefill` at 2 pressure levels (set via `--num-gpu-blocks-override` **[VERIFY]**); also calibrate the sim profile and report sim-vs-real error |
| **E7** | H4: can we infer lifecycle from real harnesses? | live | ≥ 20 scripted sessions each with OpenCode and Claude Code against vLLM via Tether. Ground truth from harness transcripts (e.g. Claude Code JSONL session logs). Report session attribution accuracy, tool-pause precision/recall, subagent detection, and miss-cause breakdown (`tether doctor`) |
| **E8** (stretch) | Versus a production scheduler | kind + llm-d | Same replay through llm-d's EPP with prefix-cache scorer on sim workers |

### 11.2 Baselines
`RoundRobin`, `LeastLoaded`, `StickySession`, `PrefixAware` (strong baseline: exact residency in the sim,
KV events on real workers), each with LRU (`noop`).

### 11.3 Metrics
- TTFT p50/p90/p99, split by resume kind (`first`, `tool`, `user`) and class (`main`, `subagent`, `side`)
- **Recomputed prefill tokens**: tokens prefilled that were resident somewhere before the request (waste)
- Cache hit rate = cached / prompt tokens
- Session completion time; total replay time (AgentX-style)
- Throughput (tokens/s per worker) at iso-TTFT
- **Retention cost:**
  - block-seconds held;
  - grants `used` / `expired` / `evicted`;
  - actuator overhead (extra requests, slot-seconds, decode tokens).
- **Fairness:** TTFT of new sessions and side calls (retention must not starve them)
- **Side traffic is measured only live (E7).** Every AgentX/Weka build filters out classifier calls
  (`max_tokens ≤ 64` with no tools: title generation, suggestion mode, security monitor) at the source
  (FINDINGS-M0 §A7). So the simulator and the replayer see essentially no `Side` traffic. Side-call
  classification, side-call handling, and `class=side` fairness are reported from E7 only. Sim and
  replay results report only `main` and `subagent`.
- Miss attribution breakdown

### 11.4 Reporting rules
- Report mean ± 95% CI over seeds (sim) or repetitions (real).
- **Show the benefit-versus-pressure curve**, not one cherry-picked point.
- Include configurations where Tether does *not* help.
- Every chart caption names its result file.

### 11.5 Charts for `docs/RESULTS.md`
1. p90 tool-resume TTFT vs KV capacity (one line per policy)
2. recomputed prefill tokens by policy (bar)
3. session completion time CDF
4. retention cost vs benefit (scatter across configs)
5. miss-attribution stacked bars per policy
6. predictor calibration curve
7. sim vs real agreement (E5/E6)

---

## 12. Testing strategy

- **Unit (core):**
  - lineage matching (extend, rerender, compaction, parallel side calls);
  - state-machine transitions (table-driven, every row of §6.1.3);
  - predictor quantiles vs known distributions;
  - retention budget invariants;
  - router scoring and hysteresis.
- **Property tests (`proptest`):**
  - retained blocks never exceed budget;
  - a session is never in two phases;
  - explicit hints always override inference;
  - the simulator conserves blocks (allocated = free + used + retained).
- **Golden tests:** tiny synthetic traces with hand-computed expected outcomes (e.g. a 2-worker, 3-session
  scenario where `tether` must beat `round_robin` by an exact token count).
- **Protocol tests:** recorded SSE fixtures for OpenAI and Anthropic streaming (tool calls, parallel tool
  calls, errors mid-stream, usage chunk). The gateway output must be byte-identical to upstream except for
  the documented headers and the removed injected-usage chunk.
- **Integration:** docker compose with 2 patched sim workers; replay 10 sessions; assert that metrics
  appear and that decisions were made.
- **Determinism:** run the sim twice in CI, then diff `summary.json`.
- **Performance:** `criterion` benches for the lineage lookup and routing decision. Gateway overhead budget:
  p99 added latency < 1 ms per request at 200 rps on a laptop (excluding upstream time).

---

## 13. Milestones and acceptance criteria

Rough effort assumes one developer working with a coding agent. Adjust freely.

**M0 — Recon and pinning (1–2 days)**
- Clone and run `llm-d-inference-sim` with `--enable-kvcache`. Confirm:
  - KV events and `cached_tokens` in usage;
  - TTFT falls with cache hits under the `per-token` latency calculator;
  - whether KV-cache mode needs the vLLM render sidecar or works with the simulated tokenizer.
- Download AgentX/Weka traces and print their schema versions. Confirm the HF repo ids and license.
- Capture raw traffic from Claude Code and OpenCode through a passthrough build (or `mitmproxy`) against the
  sim. Record session identifiers, side-call patterns, stop reasons, and how tool results come back.
- Check every **[VERIFY]** item and write `docs/FINDINGS-M0.md`.
- Scaffold the workspace, CI, `CLAUDE.md` / `AGENTS.md`, licenses.
- ✅ **Done when:** FINDINGS-M0 answers every Appendix A item, and CI is green on an empty workspace.
  An answer may be "verified by source reading at <version>; runtime check deferred to M5" when no
  runtime check was possible in M0; it must be labelled that way, and each deferred check is added to the
  M5 checklist below.

**M1 — Traces + simulator + baselines (≈1 week)**
- Build `tether-traces` (loader, filters, synthesizers) and `tether-bench stats` (E1).
- Build `tether-core` types, plus the `RoundRobin`, `LeastLoaded`, `StickySession`, `PrefixAware`
  routers.
- Build `tether-sim` with LRU, the cost profile, closed-loop concurrency, and subagents.
- ✅ **Done when:**
  - E1 report committed;
  - sim replays the full corpus at concurrency 32 in < 60 s;
  - determinism test passes;
  - baseline results committed.

**M2 — Lifecycle policy in simulation (≈1 week)**
- Build the lifecycle state machine (hint mode), pause predictor + priors, retention policy (retain/pin),
  program affinity, the `Tether` router, and the miss explainer (sim version).
- Run E2 (reduced grid first), E3, and E4.
- ✅ **Done when:** first `RESULTS.md` draft with charts 1–6 from sim data, and an honest read on H1–H3.
- **Human checkpoint.** If H1 shows no benefit anywhere, stop and discuss before building the gateway.

**M3 — Gateway core (≈1 week)**
- Build the proxy for the three endpoints with SSE tee, usage injection/removal, hints, session resolver
  (lineage inference), lifecycle from real responses, router with all policies, worker scraper, and
  metrics.
- ✅ **Done when:**
  - protocol tests pass;
  - OpenCode and Claude Code work end to end through Tether against sim workers;
  - `/__tether/sessions` shows correct phases during a live session.

**M4 — KV events, actuators, end-to-end on sim (≈1 week)**
- Build the ZMQ subscriber and index, LRU-horizon estimate, and the `touch` + `spec_prefill` actuators.
- Build the sim patch (§6.5) and the `sim_pin` actuator, the compose stack, and `tether-bench replay`.
- Run E5.
- ✅ **Done when:** E5 agreement within tolerance (or the differences are explained), and Grafana
  dashboards are committed.

**M5 — Real GPU validation (2–3 days; needs human approval for spend)**
- Launch scripts for 2 vLLM workers with pinned flags. Build the micro-benchmark for the sim cost profile.
- Run E6, plus the AIPerf cross-check on the baseline.
- Runtime checks deferred from M0 (filled in at the end of M0 from `docs/FINDINGS-M0.md`):
  - _(none yet)_
- ✅ **Done when:** E6 results and sim calibration error are committed, and every deferred M0 runtime
  check above is confirmed or corrected. Target ≤ 10 GPU-hours total.

**M6 — Live harness evaluation and explainer (≈4 days)**
- Run E7 and `tether doctor` (HTML report). Finish the dashboard.
- ✅ **Done when:** H4 is reported, and a real miss-cause breakdown for Claude Code on vLLM is documented.

**M7 — Polish and launch (≈4 days)**
- Write the README:
  - problem;
  - 30-second quickstart with docker compose (no GPU);
  - headline chart;
  - how it works;
  - honest limitations;
  - link to Cachet.
- Write `docs/ARCHITECTURE.md` and the ADRs.
- Record a demo GIF of the dashboard during a replay.
- Draft a blog post.
- Prepare the upstream contributions (§16), for human review before sending.
- ✅ **Done when:** a fresh clone → `docker compose up` → `make demo` shows the dashboard and a replay in
  under 5 minutes.

**Stretch**
- E8 (llm-d baseline on kind)
- `vllm_retention` actuator on the PR #38514 branch
- SGLang priority actuator
- Responses API (`/v1/responses`) for Codex
- exact block-hash routing (v2 index)
- multi-tenant cache isolation (per-tenant salt; see KVGov / vLLM `cache_salt`)

---

## 14. Risks and honest limitations

| Risk | Mitigation / how we report it |
|---|---|
| Benefit only appears under memory pressure | That's expected. Show the pressure curve and state where the benefit vanishes. |
| `touch` is a workaround that costs slots and may hurt under heavy load | Budgeted, pressure-gated, overhead measured and reported. `sim_pin` shows the clean API version. |
| Retention starves new sessions | Budget fraction + fairness metrics; fail the policy if new-session p90 TTFT regresses > 10% |
| Lifecycle inference misfires (side calls, parallel requests, unusual harnesses) | Hints always override; E7 measures accuracy; the decision log makes errors visible |
| Replay can't reuse decode-token KV (synthetic outputs) | Documented (§6.4); small effect given short outputs |
| Upstream projects ship the same feature | Positive outcome: contribute data/code upstream; Tether remains the engine-agnostic, harness-agnostic reference plus the eval harness |
| Sim infidelity | E5/E6 calibration and agreement reported; simulator claims labeled as such |
| Session affinity increases cross-tenant cache sharing concerns | Out of scope for v1; documented in SECURITY.md with pointers to cache isolation work |
| GPU cost creep | Human approval gate; small model; 2 instances on one GPU is acceptable |

---

## 15. Security

- Never log credentials or bodies (§0.7). Auth headers pass through untouched; Tether adds no credentials of
  its own.
- Actuator requests (touch/spec-prefill) re-send the session's own content to the **same** worker the
  session already used, with the client's original auth headers **only if** the operator enables
  `actuators.reuse_client_auth`. Otherwise they use a configured worker token. Default for local workers:
  no auth.
- Bind `127.0.0.1` by default. The dashboard and admin endpoints have no auth; don't expose them.
- The decision log contains fingerprints and token counts, never content.
- **Captured traffic** (M0 recon, E7, debugging): raw captures live outside the repo (or under a
  gitignored `captures/` directory) and are deleted once the findings they support are written. Auth
  headers are redacted at write time; `metadata.user_id` and any account ids are hashed. Only structure is
  committed — field names, header names, hashes, counts — never prompt/response content or credentials.
- Document the trust model in `SECURITY.md`, modeled on Cachet's.

---

## 16. Deliverables and launch plan (portfolio)

1. **Repo** with README, quickstart (no GPU), results, ADRs, and a dashboard GIF.
2. **Blog post / LinkedIn write-up:** "Agents re-read 96% of their context every turn — here's how much
   self-hosted serving throws away, and a gateway that keeps it". Lead with the pressure curve and the miss
   breakdown from real Claude Code on vLLM.
3. **Upstream (all human-approved first):**
   - issue + PR to `llm-d-inference-sim`: retention directive simulation + session labels (§6.5);
   - a data-backed comment on llm-d #2584, vLLM #37003/#57103, AIBrix #2814 (link results; no spam);
   - offer the lifecycle-inference rules as input to vLLM agentic-api #364.
4. **Talk-ready material:** one architecture diagram, one results chart, and a 5-minute demo script.

---

## 17. References

- vLLM blog, *vLLM x AgentX: Optimizing for Real-World Agentic Serving* (2026-09-08): https://vllm.ai/blog/2026-09-08-vllm-agentx
- SemiAnalysis InferenceX AgentX methodology: https://inferencex.semianalysis.com/agentx
- NVIDIA Dynamo, *Full-Stack Optimizations for Agentic Inference*: https://docs.nvidia.com/dynamo/v1.0.1/blog/agentic-inference
- llm-d RFC #2584: https://github.com/llm-d/llm-d/issues/2584
- AIBrix RFC #2814: https://github.com/vllm-project/aibrix/issues/2814
- vLLM RFC #37003 (retention API): https://github.com/vllm-project/vllm/issues/37003 · PR #38514
- vLLM RFC #48501 (session-centric KV): https://github.com/vllm-project/vllm/issues/48501
- vLLM RFC #57103 (programmable KV cache): https://github.com/vllm-project/vllm/issues/57103
- vLLM agentic-api #364: https://github.com/vllm-project/agentic-api/issues/364
- llm-d-inference-sim: https://github.com/llm-d/llm-d-inference-sim (see `docs/kv-cache.md`, `docs/latency-simulation.md`)
- AIPerf Weka/AgentX replay docs: https://docs.nvidia.com/aiperf/dev/tutorials/datasets-inputs/replay-weka-agentic-coding-traces
- AgentX trace corpus (canonical, 393 traces): https://huggingface.co/datasets/semianalysisai/cc-traces-weka-062126
- Older Weka trace build, schema example only: https://huggingface.co/datasets/semianalysisai/cc-traces-weka-with-subagents-051926
- Claude Code docs, *How Claude Code uses prompt caching* (prompt layering, what breaks the cache, subagents vs forks): https://code.claude.com/docs/en/prompt-caching
- *How Do AI Agents Spend Your Money?* (token cost of agentic tasks): https://arxiv.org/abs/2604.22750
- AgentKV (phase-aware KV eviction): https://arxiv.org/html/2609.14872
- KVGov (prefix-cache side channels, for the security note): https://arxiv.org/abs/2608.09225
- vLLM Claude Code integration docs: https://docs.vllm.ai/en/stable/serving/integrations/claude_code/
- Cachet: https://github.com/abhix2112/cachet

---

## Appendix A — Assumptions to verify in M0

| # | Assumption | How to check |
|---|---|---|
| A1 | llm-d-inference-sim KV-cache mode works without the render sidecar (simulated tokenizer), or the sidecar is easy to run on CPU | Run it; read `docs/kv-cache.md` + `docs/tokenization.md` at the pinned commit |
| A2 | Sim reports `cached_tokens` and TTFT drops with hits under `per-token` latency | Send the same long prompt twice; compare TTFT and usage |
| A3 | vLLM accepts `session_id` / `X-Session-ID` and emits session labels in `BlockStored` (PRs #48048, #51381) | Read code at pinned version; subscribe to events |
| A4 | vLLM flags: `--kv-events-config`, `--enable-prompt-tokens-details`, `--num-gpu-blocks-override`, `ignore_eos`, token-id prompts on `/v1/completions` | Pinned-version docs + a smoke test |
| A5 | vLLM `/v1/messages` works with the current Claude Code and returns cached-token usage | Run Claude Code against it; inspect responses (known compatibility issues exist for some Claude Code versions) |
| A6 | Metric names (`vllm:num_requests_running`, `...waiting`, `kv_cache_usage_perc`, `prefix_cache_*`) | Scrape `/metrics` |
| A7 | Weka/AgentX trace schema versions, fields (`stop`, `input_kind`, SPAWN/JOIN), HF repo ids, license | Download and inspect |
| A8 | What session identifiers Claude Code / OpenCode / Codex actually send | Traffic capture |
| A9 | Status of vLLM PR #38514 / #53423, llm-d #2584, AIBrix #2814 | Re-read before M2 and before M7 |
| A10 | Pure-Rust `zeromq` SUB interoperates with vLLM's PUB | Smoke test |
| A11 | A good small model for GPU validation with reliable tool calling on vLLM | Check vLLM recipes at the pinned version |

## Appendix B — Glossary

- **Prefill:** processing the input prompt to build the KV cache; compute-heavy; dominates agent turns.
- **Decode:** generating output tokens one at a time; memory-bandwidth-bound.
- **KV cache:** the attention keys/values per token, stored in GPU memory in fixed-size **blocks** (vLLM
  default 16 tokens).
- **Prefix caching:** reusing blocks when a new request starts with the same tokens as a previous one.
- **Eviction / LRU:** when memory is full, drop the least-recently-used unreferenced blocks.
- **Retention (retain / pin):** protecting blocks from eviction for a TTL — soft (may yield) vs strict.
- **TTFT / ITL / E2E:** time to first token / inter-token latency / end-to-end latency.
- **Session / turn:** one agent conversation; one request/response within it.
- **Tool pause:** the gap while the harness runs a tool before sending the next turn.
- **Subagent:** a child conversation spawned by the main agent, sharing its system/tool prefix.
- **Compaction:** the harness summarizing old context to shrink it, which rewrites the prefix.
- **Program:** the (model, system prompt, tool definitions) combination shared across sessions.
- **LRU horizon H(w):** roughly how long an unused block survives on worker w before eviction.
