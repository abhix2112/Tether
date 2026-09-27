# Security

Tether is a single-tenant research gateway. This file states its trust model. It will be expanded in M7,
modeled on Cachet's SECURITY.md.

## Trust model (v1)

- Tether binds to `127.0.0.1` by default. The `/__tether/*` dashboard and admin endpoints have **no
  authentication**. Do not expose them to a network you don't trust.
- **Client credentials are not forwarded to workers by default.** Tether removes the client's
  `authorization` and `x-api-key` headers before forwarding. It adds a worker's own token only if you
  configure one, via an environment variable named by `auth_token_env`; tokens never go in config files.
  Set `forward_client_auth: true` only if your workers really need the client's credentials. Coding agents
  send their real provider credentials, and a self-hosted worker should not receive them.
- Tether never logs `Authorization`, `x-api-key`, or prompt/response bodies at `info` level or above.
  Debug-level body logging is opt-in and truncated.
- The decision log contains fingerprints and token counts, never content.
- Prefix caches shared across users can leak information through timing. Tether v1 is single-tenant and
  session affinity can make this worse in a multi-tenant setup; see SPEC §14 and vLLM's `cache_salt` work.

## Captured traffic

Recon and evaluation sometimes capture raw harness traffic (for example, Claude Code's requests to learn
which session identifiers it sends). Rules:

- Raw captures stay outside the repository, or under a `captures/` directory, which is gitignored.
- Auth headers are redacted when the capture is written. `metadata.user_id` and any account ids are hashed.
- Raw captures are deleted once the findings they support are written.
- Only structure is committed: field names, header names, hashes and counts. Never prompt or response
  content, and never credentials.

## Reporting a vulnerability

Please open a private security advisory on the GitHub repository rather than a public issue.
