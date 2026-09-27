# Draft: request to SemiAnalysis for stop/input labels in the v7 Weka traces

> **Status:** draft for the owner to post after M1 (e.g. as a discussion on the
> `semianalysisai/cc-traces-weka-062126` dataset page on Hugging Face). Not sent. Check the numbers against
> the committed E1 results before posting, and link them.

---

**Title:** Request: keep `stop` / `input_types` in the v7 Weka trace builds

Hi SemiAnalysis team, and thanks for publishing the AgentX / Claude Code Weka traces. We're using
`cc-traces-weka-062126` (the 393-trace v7 set) to study KV-cache retention during agent tool pauses in
self-hosted serving (project: <link to Tether repo>).

One small request. The April build (`cc-traces-weka-042026`) carried per-request `stop`, `input_types` and
`output_types`, plus per-trace `system_tokens` / `tool_tokens`. The v5 and v7 builds don't. For
cache-policy work these fields are what tell a **tool pause** (the harness is running a tool and will
resume soon) apart from a **user wait** (the assistant ended its turn). That distinction is exactly what a
retention policy needs.

On the labelled April set, gap length alone can't recover the distinction: the p90 gap after `tool_use`
and after `end_turn` is about the same (<link to our E1 result file>). So we currently have to assume
every main-turn gap in the v7 set is a tool pause, and we test how sensitive our results are to that
assumption.

Would it be possible to include, in a future v7+ build:

1. `stop` (the response's stop reason) on each request, main and sub-agent;
2. `input_types` (or just a flag saying whether the input starts with a `tool_result`);
3. optionally, tool names for `tool_use` stops (even hashed), which would let pause-length predictors be
   keyed by tool;
4. optionally, `system_tokens` / `tool_tokens` per trace (or per sub-agent group).

We'd be glad to share our analysis and results back. Thanks again for the dataset!
