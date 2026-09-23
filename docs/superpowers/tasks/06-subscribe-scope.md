# 06 — A `subscribe:*` scope

**Kind:** Bounded · **Source:** `docs/limitations.md` #14

## The gap

Managing a subscription reuses the token-management rule: admin, curator, or the owning
deployment's credential. There is no scope for it, so no credential can be issued that may
subscribe and **nothing else** — a downstream consumer that only wants webhooks must be handed a
credential that can also advertise.

## Where

- `src/auth/mod.rs` — `SCOPE_*` constants and `ALL_SCOPES` (~l.30–45), `require_scope`.
- `src/api/subscriptions.rs` — the authorisation check on create / list / delete / ack.
- `frontend/src/routes/Tokens.tsx` — scope checkboxes when minting a token.
- `docs/api/subscriptions.md`, `docs/api/conventions.md` "Roles and scopes", and
  `docs/specs/2026-08-31-artifact-subscriptions.md` §8.3 reasoning.
- MCP: `src/mcp/tools.rs` filters tools by authority — check whether subscription tools exist.

## Decide (with the user, briefly)

- Name: `subscribe:artifacts`, or `subscribe:*` as the limitations entry says. Match the
  `verb:noun` style of the existing scopes.
- Does an existing deployment token without the scope keep subscribing (backwards compatible),
  or must it be re-issued? Recommend: the owning deployment keeps working; the scope *adds* a
  narrower way in, it takes nothing away.

## Done looks like

- A token with only the new scope, bound to a deployment, can create/list/ack/delete that
  deployment's subscriptions and is refused `advertise`, `register`, and other deployments'
  subscriptions (tests in `tests/subscriptions.rs`).
- Existing behaviour for admin, curator and full deployment tokens unchanged — the current suite
  stays green without edits.
- The scope appears in the token UI and the docs; #14 closed.
