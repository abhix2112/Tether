# Tether — Owner's Learning Guide (learn while building)

This guide is for **you, the owner** — not for the coding agent. The agent builds Tether from `SPEC.md`;
this file makes sure you understand every concept well enough to explain it in an interview, defend the
design, and debug it yourself.

Put it at `docs/LEARNING.md` in the repo. All links were checked on 2026-09-27.

---

## How to use this guide

- **Follow the milestone plan (§8).** Learn each concept just before the agent builds the part that uses
  it. You'll understand the code faster, and the agent's choices will make sense.
- **Each concept has:**
  - a plain-language explanation;
  - **Where in Tether** (the SPEC section that uses it);
  - 🎥 **Watch** (a video), 📖 **Read** (an article/doc), 🔬 **Go deeper** (paper or source code);
  - ✅ **Check yourself:** questions you should be able to answer without notes. If you can't, rewatch.
- **Rule of thumb:** one video + one read per concept is enough to start. Go deeper only on the ⭐ concepts:
  they are the heart of Tether and what interviewers will probe.
- After each milestone, write a 5-line "what I learned" note (it becomes your LinkedIn/blog material).

---

## 0. The whole project in one picture

What happens on one turn of a coding agent, and where Tether sits:

```
Agent (Claude Code)          Tether gateway                         GPU worker (vLLM)
───────────────────          ──────────────                         ─────────────────
turn 7: sends ALL 120K  ──▶  "this is session S, turn 7,     ──▶    prefill: 118K tokens already in
tokens of history +          it continues turn 6, which ran         KV cache (cache hit!) → only
2K new tokens (tool          on worker B" → route to B              compute the 2K new ones → fast
result)                                                              first token
                             reads the reply as it streams:   ◀──   decode: writes 300 tokens,
                             "it ended with a tool call (Bash)"      ends with a tool call
agent runs `npm test`        predicts: "Bash pauses ≈ 20 s here";
for 20 seconds …             protects S's 120K tokens of KV on B
                             from eviction for ~24 s                 other sessions arrive; without
turn 8 arrives ─────────▶    route to B again                ──▶     protection, LRU would have
                                                                     evicted S's cache → recompute
                                                                     120K tokens (slow, expensive)
```

Everything below explains one piece of that picture.

---

## Part 1 — How LLM inference works (the foundation)

### 1.1 Transformers and attention
A transformer turns tokens into vectors, and each layer lets every token "look at" earlier tokens
(attention) to decide what comes next. For Tether you don't need the math. You do need to know that
attention on a new token needs information (keys and values) from **all previous tokens**.

- **Where in Tether:** background for everything; explains why the KV cache exists.
- 🎥 [3Blue1Brown — Attention in transformers, visually explained](https://www.youtube.com/watch?v=eMlx5fFNoYc) (26 min)
- 📖 [Jay Alammar — The Illustrated Transformer](https://jalammar.github.io/illustrated-transformer/) · [The Illustrated GPT-2](https://jalammar.github.io/illustrated-gpt2/) (decoder-only, like modern LLMs)
- 🔬 [Karpathy — Let's build GPT from scratch](https://www.youtube.com/watch?v=kCc8FmEb1nY) (2 h, code-along; do it once in your life)
- ✅ Why does generating token #1001 need something from tokens #1–#1000?

### 1.2 Tokens and tokenization
Models read **tokens** (sub-word pieces), not characters. The same text always becomes the same tokens —
but tiny text changes (a timestamp, whitespace) change the tokens from that point on.

- **Where in Tether:** block hashing and the synthetic prompts in the replayer (§6.4); "prefix divergence"
  in the miss explainer (§6.1.7).
- 🎥 [Karpathy — Let's build the GPT Tokenizer](https://www.youtube.com/watch?v=zduSFxRajkE) (watch the first 30 min)
- ✅ If the system prompt contains today's date, what happens to prefix caching across days?

### 1.3 ⭐ Prefill vs decode, and the KV cache
- **Prefill:** the model processes the whole prompt at once and stores keys/values (the **KV cache**) for
  every token. It is compute-heavy, and its cost grows with prompt length.
- **Decode:** the model generates output tokens one at a time, each reusing the KV cache. It is
  memory-bandwidth-heavy.
- **Why agents care:** the inputs are huge (~142K tokens median in AgentX) and the outputs are tiny
  (~444). So *prefill dominates*, and skipping prefill via cache reuse is the main win.

- **Where in Tether:** everything; the simulator's cost model (§6.3) is literally
  `prefill_time = overhead + uncached_tokens × t_prefill`.
- 🎥 [Umar Jamil — LLaMA explained: KV-Cache, RoPE, GQA…](https://www.youtube.com/watch?v=Mn_9W1nCFLo) (focus on the KV-cache part)
- 📖 [NVIDIA — Mastering LLM Techniques: Inference Optimization](https://developer.nvidia.com/blog/mastering-llm-techniques-inference-optimization)
- 🔬 [kipply — Transformer Inference Arithmetic](https://kipp.ly/transformer-inference-arithmetic/) (the best explanation of *why* prefill is compute-bound and decode is memory-bound)
- ✅ Why is "time to first token" mostly prefill time? Why does a cache hit mostly help TTFT, not the
  speed of the tokens after it?

### 1.4 ⭐ How big is the KV cache? (the reason GPUs run out of memory)
KV cache per token = `2 (K and V) × layers × kv_heads × head_dim × bytes_per_value`.

**Worked example** (illustrative config: 36 layers, 8 KV heads, head dim 128, fp16 = 2 bytes):
`2 × 36 × 8 × 128 × 2 = 147,456 bytes ≈ 0.14 MB per token`.
One agent session at 142K tokens ≈ **~21 GB of KV cache**. A handful of sessions fill a GPU. That is why
eviction happens, and why *which* session's cache survives matters so much.

- **Where in Tether:** block budgets and retention cost (§6.1.5); the pressure sweep in the evaluation
  (§11).
- 📖 Same kipply article above (KV cache section).
- ✅ Redo the math for your GPU-validation model once M5 picks it (read the numbers from its
  `config.json`).

### 1.5 Continuous batching
Old servers waited for a whole batch to finish. Modern engines add and remove requests from the batch at
every step ("iteration-level scheduling"), which is why throughput jumped. Your simulator models a
simplified version (max concurrent sequences + FCFS).

- **Where in Tether:** simulator scheduler (§6.3); the `max_num_seqs` load signal in the router (§6.1.6).
- 📖 [Anyscale — How continuous batching enables 23x throughput](https://www.anyscale.com/blog/continuous-batching-llm-inference)
- 🔬 [Orca paper (OSDI '22)](https://www.usenix.org/conference/osdi22/presentation/yu)
- ✅ Why does one very long prefill slow down everyone else's decode on the same GPU?

### 1.6 ⭐ PagedAttention and KV blocks (vLLM)
vLLM stores the KV cache in fixed-size **blocks** (16 tokens by default) that don't need to be contiguous
— like virtual-memory pages in an OS. That removes waste and makes sharing and eviction happen at block
granularity.

- **Where in Tether:** everything is counted in blocks. The KV events Tether listens to say "block stored"
  / "block removed" (§6.2.4).
- 📖 [vLLM launch blog — Easy, Fast, and Cheap LLM Serving with PagedAttention](https://vllm.ai/blog/2023-06-20-vllm)
- 🎥 [Fast LLM Serving with vLLM and PagedAttention (talk)](https://www.youtube.com/watch?v=5ZlavKF_98U) · [SOSP '23 paper talk](https://www.youtube.com/watch?v=UdNocRPQS3Y)
- 🔬 Paper: [Efficient Memory Management for LLM Serving with PagedAttention](https://arxiv.org/abs/2309.06180) · Code to read: [nano-vllm (~1,200 lines)](https://github.com/GeeeekExplorer/nano-vllm)
- ✅ What problem do blocks solve that "one big array per request" doesn't?

### 1.7 ⭐ Prefix caching (hash chains) and RadixAttention
- **vLLM:** each full block gets a hash computed from *its tokens plus the previous block's hash* (a hash
  chain). A new request walks its blocks. As long as the hashes are already in the cache, it skips them.
  The first mismatch ends the reuse; everything after it is recomputed.
- **SGLang:** the same idea, stored as a radix tree of token prefixes (RadixAttention).
- **Where in Tether:** the core effect Tether protects. It is also why the miss explainer asks "at which
  message did the prefix diverge?" (§6.1.7).
- 📖 [vLLM design doc — Automatic Prefix Caching](https://docs.vllm.ai/en/stable/design/prefix_caching/) (read fully — short and exact)
- 📖 [LMSYS — Fast and Expressive LLM Inference with RadixAttention and SGLang](https://www.lmsys.org/blog/2024-01-17-sglang/)
- 🔬 Code: [mini-sglang](https://github.com/sgl-project/mini-sglang) ([blog](https://www.lmsys.org/blog/2025-12-17-minisgl/)) — read `radix_cache`
- ✅ Why is it a *chain* (each hash includes the parent's)? What happens to caching if message #3 of 40
  changes?

### 1.8 ⭐ Eviction, LRU, and retention/pinning
When memory is full, the engine frees the **least-recently-used** blocks that no running request is
using. LRU knows only "when was this last used". It doesn't know "this session is just waiting 20 s for a
test run".

- **Retain (soft):** evict these later than normal, but they may still go under pressure.
- **Pin (strict):** don't evict until the TTL expires.

These are exactly the semantics being proposed upstream (vLLM RFC #37003, #57103) and simulated by
Tether's sim patch.

- **Where in Tether:** retention policy (§6.1.5), actuators (§6.2.5), sim patch (§6.5).
- 📖 [Wikipedia — Cache replacement policies](https://en.wikipedia.org/wiki/Cache_replacement_policies) (LRU, LFU, and friends)
- 📖 [NVIDIA Dynamo — Full-Stack Optimizations for Agentic Inference](https://docs.nvidia.com/dynamo/v1.0.1/blog/agentic-inference) (the "Layer 3: KV Cache Management" part is Tether's motivation in one page)
- ✅ Why is LRU a bad fit when a session goes quiet for 20 s but will definitely return?

### 1.9 Chat templates and tool calling (the wire formats Tether reads)
Chat APIs send a list of messages. The server's **chat template** turns them into one long token sequence.
Tool calling means the model replies with "call tool X with args Y" instead of text:
- OpenAI marks it with `finish_reason: "tool_calls"`;
- Anthropic marks it with `stop_reason: "tool_use"`.

That end-of-response signal is how Tether knows a tool pause is starting.

- **Where in Tether:** response inspector and lifecycle state machine (§6.1.3, §6.2.1); the
  `assistant_rerender` miss cause (§6.1.7).
- 📖 [Hugging Face — Chat templates](https://huggingface.co/docs/transformers/chat_templating)
- 📖 [Anthropic — Tool use overview](https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview) · [How tool use works](https://platform.claude.com/docs/en/agents-and-tools/tool-use/how-tool-use-works) · [Stop reasons](https://platform.claude.com/docs/en/build-with-claude/handling-stop-reasons)
- 📖 [OpenAI — Function calling](https://developers.openai.com/api/docs/guides/function-calling)
- ✅ If the template drops the model's `<think>` text when re-rendering the previous assistant turn, why
  does the next turn miss the cache right after that point?

### 1.10 ⭐ Prompt caching in hosted APIs, and how Claude Code uses it
Anthropic and OpenAI do prefix caching for you. Claude Code orders its prompt so stable parts come first
(system prompt + tools, then project context, then the conversation). This page lists exactly which
actions break the cache — model switch, tool changes, compaction — and how subagents vs forks share
cache. That is ground truth for Tether's lifecycle rules.

- **Where in Tether:** session/lineage rules (§6.1.2), subagent handling, the miss explainer.
- 📖 [Claude Code docs — How Claude Code uses prompt caching](https://code.claude.com/docs/en/prompt-caching) (**must read**)
- 📖 [Claude blog — Lessons from building Claude Code: Prompt caching is everything](https://claude.com/blog/lessons-from-building-claude-code-prompt-caching-is-everything)
- 📖 [Claude API — Prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)
- ✅ Why doesn't a regular Claude Code subagent read the parent's cache, while a fork does? What does
  that mean for where Tether should route each?

---

## Part 2 — Serving at scale (the systems layer)

### 2.1 ⭐ Latency metrics: TTFT, ITL, E2E, percentiles, goodput
- **TTFT** = time to first token; **ITL** = time between tokens; **E2E** = the whole request.
- Report **p50/p90/p99**, not averages: users feel the slow tail.
- **Goodput** = requests per second that meet the latency targets.
- Measure latency carefully: the "coordinated omission" trap makes load tests lie.

- **Where in Tether:** all evaluation (§11); replayer measurements (§6.4).
- 🎥 [Gil Tene — Understanding Latency and Application Responsiveness](https://www.youtube.com/watch?v=YpBwt-rO07o) · [How NOT to Measure Latency (InfoQ)](https://www.infoq.com/presentations/latency-pitfalls/)
- 📖 [Dean & Barroso — The Tail at Scale](https://research.google/pubs/the-tail-at-scale/)
- 📖 [Hao AI Lab — Throughput is Not All You Need (goodput)](https://haoailab.com/blogs/distserve/)
- ✅ Why is "average TTFT improved 10%" a weak claim? What's coordinated omission?

### 2.2 Load balancing basics
- **Round robin:** takes turns.
- **Least loaded:** send to the idlest worker.
- **Consistent hashing:** the same key (session) always goes to the same worker, even as workers come and
  go.
- **Power of two choices:** pick 2 random workers and take the less loaded one. It is surprisingly close to
  optimal.

These are Tether's baselines.

- **Where in Tether:** the `Router` trait and baselines (§6.1.6, §11.2).
- 📖 [ByteByteGo — Consistent hashing explained](https://bytebytego.com/guides/consistent-hashing/)
- 🔬 [Mitzenmacher — The Power of Two Choices in Randomized Load Balancing](https://www.eecs.harvard.edu/~michaelm/postscripts/tpds2001.pdf)
- 📖 [vLLM Router release blog](https://vllm.ai/blog/2025-12-13-vllm-router-release) (consistent hashing for session stickiness in practice)
- ✅ Why is pure session stickiness dangerous when one worker gets overloaded? (That's why Tether has
  hysteresis and a load penalty.)

### 2.3 ⭐ KV-cache-aware routing
Routers that know which worker holds which cached blocks, and score workers by "cache overlap minus load".
Tether's strong baseline (`PrefixAware`) is this. Tether adds lifecycle knowledge on top.

- **Where in Tether:** router scoring (§6.1.6), KV index (§6.2.4).
- 📖 [llm-d — KV-Cache Wins You Can See](https://llm-d.ai/blog/kvcache-wins-you-can-see) (**must read**)
- 📖 [llm-d docs — Prefix-cache aware routing](https://llm-d.ai/docs/dev/architecture/advanced/kv-management/prefix-cache-aware-routing) · [Red Hat — Master KV cache aware routing with llm-d](https://developers.redhat.com/articles/2025/10/07/master-kv-cache-aware-routing-llm-d-efficient-ai-inference)
- 📖 NVIDIA Dynamo agentic blog (link in §1.8) — "Layer 2: The Router"
- ✅ What information does a router need to know "worker B has 90% of this prompt cached"? Where does it
  come from? (Answer: KV events.)

### 2.4 Prefill/decode disaggregation (context only — not built in v1)
Big deployments run prefill and decode on different GPUs and ship the KV cache between them. You should
know it exists and why; interviewers will ask.

- 📖 [Hao AI Lab — DistServe blog](https://haoailab.com/blogs/distserve/) · [18 months later retrospective](https://haoailab.com/blogs/distserve-retro/)
- 🔬 [DistServe paper](https://arxiv.org/abs/2401.09670)
- ✅ Why does mixing long prefills with decodes on one GPU hurt ITL?

### 2.5 KV offloading and multi-tier caches (context)
KV can spill from GPU memory to CPU RAM, local SSD, or a shared store (Mooncake, LMCache). This is an
alternative or complement to Tether's "keep it on the GPU" approach.

- 📖 [vLLM × Mooncake — Serving Agentic Workloads at Scale](https://vllm.ai/blog/2026-05-06-mooncake-store)
- 🔬 [Mooncake paper](https://arxiv.org/abs/2407.00079)
- ✅ When would offloading to CPU beat retention on GPU? (Think: how long the pause is vs. how long the
  transfer takes.)

### 2.6 ⭐ Why agent traffic is different
Long sessions, a huge re-sent context, tiny outputs, tool pauses, subagents, and most of the cost in
*input* tokens.

- **Where in Tether:** the whole problem statement (§3).
- 📖 [vLLM — vLLM × AgentX: Optimizing for Real-World Agentic Serving](https://vllm.ai/blog/2026-09-08-vllm-agentx) (**must read**)
- 📖 [SemiAnalysis — AgentX methodology & datasets](https://inferencex.semianalysis.com/agentx) · [Agentic benchmark guide](https://inferencex.semianalysis.com/blog/agentic-benchmark-agent-benchmark-guide)
- 🔬 [How Do AI Agents Spend Your Money?](https://arxiv.org/abs/2604.22750) (why input tokens dominate agent costs)
- ✅ Quote the four AgentX numbers from SPEC §3.1 from memory and explain what each implies for caching.

### 2.7 ⭐ The research frontier Tether sits in (agent-aware KV management)
Read these *after* M1, when the problem is concrete for you. They are the ideas you'll compare against in
your blog post.

- 🔬 [Continuum — KV cache time-to-live for multi-turn agents](https://arxiv.org/abs/2511.02230) (TTL retention during tool calls: the closest idea to Tether's retention)
- 🔬 [KVFlow — prefix caching for multi-agent workflows](https://arxiv.org/abs/2507.07400) (needs the workflow graph declared up front; Tether infers it instead)
- 🔬 [AgentKV — phase-aware KV eviction](https://arxiv.org/html/2609.14872)
- 📖 Upstream discussions (read the issue text; skim the comments): [llm-d #2584](https://github.com/llm-d/llm-d/issues/2584) · [vLLM RFC #37003 retention API](https://github.com/vllm-project/vllm/issues/37003) · [vLLM RFC #57103 programmable KV cache](https://github.com/vllm-project/vllm/issues/57103) · [agentic-api #364](https://github.com/vllm-project/agentic-api/issues/364)
- ✅ In one sentence each: what does Continuum assume that Tether doesn't? What does KVFlow assume?

---

## Part 3 — The algorithms inside Tether

### 3.1 Hashing and fingerprints
Tether fingerprints each message (fast non-cryptographic hash, xxh3) to tell whether a new request
*extends* a known session. It's the same "chain" idea as prefix caching, applied to messages instead of
tokens.

- **Where in Tether:** lineage matching (§6.1.2).
- 📖 The vLLM prefix-caching doc (§1.7) shows the hash-chain idea.
- ✅ Why normalize JSON key order before hashing tool arguments?

### 3.2 Finite state machines
Each session is in exactly one phase (`Generating`, `ToolPause`, `UserWait`, …), and events move it
between phases. Rust enums make illegal states hard to represent.

- **Where in Tether:** lifecycle tracker (§6.1.3).
- 📖 [Pretty State Machine Patterns in Rust](https://hoverbear.org/blog/rust-state-machine-pattern/) · [A Fistful of States](https://deislabs.io/posts/a-fistful-of-states/)
- ✅ Draw the lifecycle table from SPEC §6.1.3 as a diagram from memory.

### 3.3 ⭐ Online statistics: histograms, quantiles, CDFs, decay
To predict "how long will this Bash pause last?", Tether keeps a small histogram of past pauses per tool,
reads quantiles off it (the p90 pause), and lets old observations fade (exponential decay) so it adapts.

- **Where in Tether:** pause predictor (§6.1.4).
- 📖 [Wikipedia — Exponential smoothing (EWMA)](https://en.wikipedia.org/wiki/Exponential_smoothing)
- 🔬 [DDSketch paper (Datadog)](https://arxiv.org/abs/1908.10693) · [the morning paper summary](https://blog.acolyer.org/2019/09/06/ddsketch/) (log-spaced buckets, the same idea as Tether's histogram)
- 📖 [Prometheus — Histograms and summaries](https://prometheus.io/docs/practices/histograms/) (same concept, observability side)
- ✅ Why use log-spaced buckets for durations that range from 50 ms to 5 minutes?

### 3.4 Priors and shrinkage (empirical Bayes)
A brand-new tool has 3 observations, which is too few to trust. Tether blends its estimate with a global
prior from the AgentX traces, weighted `n / (n + k)`. As data arrives, the tool's own estimate takes over.

- **Where in Tether:** predictor prior (§6.1.4).
- 📖 [Variance Explained — Understanding empirical Bayes estimation](http://varianceexplained.org/r/empirical_bayes_baseball/) (baseball example; the intuition transfers directly)
- ✅ With `k = 20`, how many observations does a tool need before its own data gets 50% of the weight?

### 3.5 Calibration of predictions
"p90 = 24 s" should mean 90% of pauses really end within 24 s. Checking that is **calibration**, and it's
experiment E4.

- 📖 [scikit-learn — Probability calibration](https://scikit-learn.org/stable/modules/calibration.html) (read the concept and the reliability-diagram picture)
- ✅ If only 70% of pauses end within the predicted p90, is the predictor over- or under-confident, and
  what happens to cache retention?

### 3.6 ⭐ Cost–benefit decisions under a budget
Retaining a session costs memory × time. It saves recompute × probability of resuming. Tether retains
when benefit > cost, caps total retention (a budget), and evicts the lowest "value per block" first. That
is the greedy approach to a knapsack problem, and the memory "price" rising with usage is a **shadow
price**.

- **Where in Tether:** retention policy (§6.1.5).
- 📖 [Wikipedia — Knapsack problem (greedy approximation)](https://en.wikipedia.org/wiki/Knapsack_problem) · [Shadow price](https://en.wikipedia.org/wiki/Shadow_price)
- ✅ Why does a budget cap protect new sessions even when every individual retention decision looks
  "worth it"?

### 3.7 The LRU horizon (a Tether-specific idea)
Under LRU, an unused block survives roughly `capacity ÷ allocation rate` seconds. That's a Little's-law
style estimate. Tether uses it to guess whether a paused session's cache is about to be evicted, and when
to "touch" it.

- **Where in Tether:** router survival model and the touch actuator (§6.1.6, §6.2.5).
- 📖 [Wikipedia — Little's law](https://en.wikipedia.org/wiki/Little%27s_law)
- ✅ A worker holds 10,000 blocks and allocates 500 blocks/s. About how long does an idle block last? Is
  a 30 s tool pause safe?

### 3.8 ⭐ Discrete-event simulation
Instead of running real time, keep a priority queue of future events ("request arrives at t=3.2 s",
"prefill finishes at t=3.9 s"). Pop the earliest, update state, push new events. A 2-hour workload
replays in seconds, and the same seed gives the same result. This is how you test policies without GPUs.

- **Where in Tether:** `tether-sim` (§6.3).
- 📖 [A gentle introduction to discrete-event simulation](https://softwaresim.com/blog/a-gentle-introduction-to-discrete-event-simulation/)
- 📖 [SimPy in 10 minutes](https://simpy.readthedocs.io/en/stable/simpy_intro/) (Python, but the concepts are identical)
- ✅ Why must the simulator never read the wall clock? What breaks if two events have the same time and
  no tie-break rule?

---

## Part 4 — The engineering stack

### 4.1 Rust and async (Tokio)
- 📖 [The Rust Book](https://doc.rust-lang.org/book/) (ch. 1–10, 13, 15, 16 if you're rusty)
- 📖 [Tokio tutorial](https://tokio.rs/tokio/tutorial)
- 🎥 [Jon Gjengset — Decrusting the tokio crate](https://www.youtube.com/watch?v=o2ob8zkeq2s)
- **Where in Tether:** the whole gateway (§6.2). Your own Cachet code is the best warm-up: re-read
  `src/proxy.rs`.
- ✅ What's the difference between `tokio::spawn` and just `.await`-ing a future?

### 4.2 HTTP proxying and streaming (SSE)
LLM responses stream as **Server-Sent Events**. Tether must forward each chunk immediately *and* read it
("tee") to spot tool calls and usage.

- 📖 [MDN — Using server-sent events](https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events/Using_server-sent_events)
- 📖 [axum examples](https://github.com/tokio-rs/axum/tree/main/examples) · your Cachet `proxy.rs` (streaming tee)
- **Where in Tether:** ingress/proxy (§6.2.1).
- ✅ Why must the proxy never buffer the whole stream before forwarding?

### 4.3 Pub/sub messaging: ZeroMQ and MessagePack
vLLM and the llm-d simulator publish "block stored/removed" events over a ZeroMQ PUB socket, encoded as
MessagePack (compact binary JSON). Tether subscribes.

- 📖 [ZeroMQ Guide — ch. 2 Sockets and Patterns](https://zguide.zeromq.org/docs/chapter2/) (pub-sub sections) · [ch. 5 Advanced Pub-Sub](https://zguide.zeromq.org/docs/chapter5/)
- 📖 [MessagePack](https://msgpack.org/)
- 📖 `docs/kv-cache.md` in [llm-d-inference-sim](https://github.com/llm-d/llm-d-inference-sim) (the exact event format)
- **Where in Tether:** KV event subscriber (§6.2.4).
- ✅ What happens to a PUB/SUB subscriber that connects late? (That's why the sim has a replay endpoint.)

### 4.4 Observability: Prometheus, Grafana, tracing
- 📖 [Prometheus — Metric types](https://prometheus.io/docs/concepts/metric_types/) · [Histograms and summaries](https://prometheus.io/docs/practices/histograms/)
- 🎥 [Jon Gjengset — Decrusting the tracing crate](https://www.youtube.com/watch?v=21rtHinFA40)
- **Where in Tether:** metrics and dashboards (§6.2.6).
- ✅ Counter vs gauge vs histogram: which one is `tether_retained_blocks`? Which is `tether_ttft_seconds`?

### 4.5 Testing: property-based and deterministic simulation testing
- 📖 [Proptest book](https://altsysrq.github.io/proptest-book/) · [Luca Palmieri — Intro to property-based testing in Rust](https://www.lpalmieri.com/posts/an-introduction-to-property-based-testing-in-rust/)
- 🎥 [Will Wilson — Testing Distributed Systems with Deterministic Simulation](https://www.youtube.com/watch?v=4fFDFbi3toc) (FoundationDB; the same philosophy as Tether's pure core + deterministic sim)
- **Where in Tether:** testing strategy (§12).
- ✅ Write one invariant for Tether in plain English that a property test could check.

### 4.6 Benchmarking methodology (how not to fool yourself)
- 📖 [Brendan Gregg — Active Benchmarking](https://www.brendangregg.com/activebenchmarking.html) · [Benchmarking checklist](https://www.brendangregg.com/blog/2018-06-30/benchmarking-checklist.html)
- 📖 [Wikipedia — Bootstrapping (statistics)](https://en.wikipedia.org/wiki/Bootstrapping_(statistics)) (for confidence intervals across seeds)
- **Where in Tether:** evaluation protocol and reporting rules (§11.4).
- ✅ Why does SPEC insist on showing the whole "benefit vs memory pressure" curve instead of the best
  point?

### 4.8 Supply-chain hygiene: pin by hash, verify downloads (added in M0)
A version tag like `v7.0.1` can be moved to point at different code. A commit SHA or a file's SHA-256
can't. Tether pins CI actions to full commit SHAs, and every downloaded binary, model or trace file is
checked against a published SHA-256 before use (see `docs/FINDINGS-M0.md` → Pins).

- **Where in Tether:** `.github/workflows/ci.yml`, `rust-toolchain.toml`, `Cargo.lock`, and the pinned
  dataset/model revisions in `docs/FINDINGS-M0.md`.
- 📖 [GitHub Docs — Secure use reference (pin actions to a full-length commit SHA)](https://docs.github.com/en/actions/reference/security/secure-use)
- ✅ Why is `uses: actions/checkout@v7` riskier than `uses: actions/checkout@<40-char SHA>`? What does
  `Cargo.lock` pin that `Cargo.toml` doesn't?

### 4.7 Docker Compose, Kubernetes (stretch), Go (sim patch), ADRs
- 📖 [Docker Compose docs](https://docs.docker.com/compose/) — the local stack (§9)
- 📖 [Kubernetes concepts overview](https://kubernetes.io/docs/concepts/overview/) — only for the llm-d stretch goal
- 📖 [A Tour of Go](https://go.dev/tour/) — enough Go to read and patch the llm-d simulator (§6.5)
- 📖 [Michael Nygard — Documenting Architecture Decisions](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions) — how to write the ADRs in `docs/adr/`

---

## Part 5 — Security note you should understand

**Prefix-cache timing side channels.** If different users share a prefix cache, one can time responses
to guess another's prompt: a fast first token means "someone already sent this prefix". Tether v1 is
single-tenant, but you must be able to explain the risk and the fix (per-tenant cache salt).

- 🔬 [KVGov — Preventing timing side-channel leakage in multi-tenant LLM inference](https://arxiv.org/abs/2608.09225)
- 📖 [vLLM RFC — Cache salting for secure prefix caching](https://github.com/vllm-project/vllm/issues/16016)
- ✅ Why would session affinity plus aggressive retention make this risk worse in a multi-tenant
  deployment?

---

## Part 6 — Learn by reading real source code (highest return per hour)

| Read | Why | Time |
|---|---|---|
| Your own Cachet `src/proxy.rs` | The streaming proxy pattern Tether reuses | 1 h |
| [nano-vllm](https://github.com/GeeeekExplorer/nano-vllm) — block manager + scheduler | See paging, prefix caching and batching in ~1,200 lines | 3 h |
| [mini-sglang](https://github.com/sgl-project/mini-sglang) — radix cache | The other way to do prefix reuse | 2 h |
| llm-d-inference-sim `docs/kv-cache.md`, `docs/latency-simulation.md` | The testbed you'll patch; its TTFT model | 1 h |
| [vLLM prefix caching design doc](https://docs.vllm.ai/en/stable/design/prefix_caching/) | The exact rules for hits and eviction | 45 min |
| [llm-d precise prefix-cache routing guide](https://github.com/llm-d/llm-d/tree/main/guides/precise-prefix-cache-routing) | How a production router consumes KV events | 1 h |

---

## Part 7 — Interview-ready questions (you're done when you can answer all of these)

1. Walk me through what happens inside vLLM when turn 8 of an agent session arrives.
2. Why is prefill the bottleneck for coding agents but not for chatbots?
3. How big is the KV cache for a 100K-token session on model X? Show the math.
4. How does vLLM decide a request can reuse cached blocks? What breaks reuse?
5. Why does LRU fail agent workloads? What did you do instead, and what does it cost?
6. How does Tether know a session is paused for a tool, without any change to the agent?
7. How do you predict pause length? How do you know the prediction is any good?
8. How do you prevent retention from starving new users?
9. What was your strongest baseline, and why is it fair?
10. Where did Tether *not* help, and why?
11. How did you validate that the simulator matches real GPUs?
12. What would change if the engine had a real retention API (vLLM #37003)?
13. What's the security risk of sharing prefix caches across tenants?
14. Why a pure, deterministic core crate shared by the simulator and the gateway?

---

## Part 8 — Milestone-by-milestone learning plan

| Milestone | Learn *before/while* the agent builds it | Your hands-on task |
|---|---|---|
| **M0** Recon | 1.1–1.3, 1.9, 1.10, 2.6 | Run the llm-d simulator yourself; send one request twice and watch `cached_tokens` and TTFT change |
| **M1** Traces + sim + baselines | 1.4–1.8, 2.1, 2.2, 3.8, 4.1 | Read the AgentX workload stats the agent produces; explain each number |
| **M2** Lifecycle policy in sim | 1.8, 2.7, 3.2–3.7 | **Implement the pause predictor or the retention policy yourself** (the agent writes the tests first) |
| **M3** Gateway core | 1.9, 4.1, 4.2, 4.4 | Point Claude Code / OpenCode at the gateway and watch `/__tether/sessions` change phase live |
| **M4** KV events + actuators | 1.6, 1.7, 2.3, 4.3, 4.5 | Subscribe to KV events with a 20-line script; watch blocks get stored/removed |
| **M5** Real GPU | 1.3, 1.4, 2.4, 2.5, 4.6 | Redo the KV-size math for the chosen model; predict the result before running |
| **M6** Live harness eval | 1.10, 5 | Run `tether doctor` on your own Claude Code session; explain every miss |
| **M7** Launch | Part 7 | Write the blog post; answer the Part 7 questions out loud |

**Tip:** keep a `docs/progress/owner-notes.md`. After each milestone, write 5 lines: what you learned,
what surprised you, one diagram. That file becomes your blog post and your interview story.
