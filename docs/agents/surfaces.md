# Agent-facing surfaces

Three of them, in increasing order of how much the client has to know in advance.

| | For a client that |
|---|---|
| [`/llms.txt`](#llmstxt) | has just been given a URL and knows nothing. |
| [Markdown representations](#markdown-representations) | can fetch a URL and read prose. |
| [The MCP server](../mcp.md) | would rather call tools than compose URLs. |

None of them is a separate copy of the data. That is the constraint the whole design here is
built around: a prose rendering that is generated from a different source than the RDF will
disagree with it, and the disagreement will be invisible.

## `/llms.txt`

`GET /llms.txt` follows the [llmstxt.org] convention: what the registry is, how to read any
record without an RDF parser, the entry points, and a link to every record in the catalogue.

```bash
curl https://registry.example.org/llms.txt
```

It lists software and deployments in full up to a limit, because those are a stable set that
changes by the week and a small registry is worth listing whole. Artifacts and runs get a recent
window instead — a single busy pipeline produces more of them in a day than the catalogue holds
in a year, and listing them the same way would bury the parts of the file that orient a reader
under a wall of near-identical rows. Everything else is one paged request away, and the file says
so.

It is public whenever reads are public. A file whose entire purpose is to tell an unfamiliar
client how to read the registry is worth nothing behind a credential the client does not yet
know it needs.

## Markdown representations

Every record IRI serves Markdown. Append `.md`, or send `Accept: text/markdown`:

```bash
curl -H 'Accept: text/markdown' https://registry.example.org/software/01a05…
curl https://registry.example.org/software/01a05….md      # the same bytes
```

It is a *representation*, not a second copy — the same graph through the same code path as the
Turtle — so the prose cannot drift from the RDF.

It is also where the registry states the things a client otherwise gets wrong, in prose, at the
point of use:

- that `deployable: false` means there is no endpoint to call, rather than that the endpoint is
  missing from the record;
- that a peer's record is a cached stub with a timestamp, not this registry's own claim;
- that a withdrawn record still resolves, and is withdrawn;
- that vocabulary terms must be looked up rather than recalled.

Every response also carries Signposting `Link` headers including `rel="alternate";
type="text/markdown"`, so the Markdown is discoverable without knowing the `.md` convention in
advance.

## The MCP server

The registry speaks the Model Context Protocol on `/mcp`, on its own web server, behind the same
credentials as the REST API. Nothing needs installing.

```
claude mcp add --transport http tar-registry https://registry.example.org/mcp
```

The full chapter is [The hosted MCP server](../mcp.md). The **Connect** tab in the UI prints the
copy-paste setup for the common clients, built from that registry's own URL rather than a
placeholder.

## Why the registry refuses guesses

An agent asked to fill in a form will fill it in. A guessed type IRI or a confident licence for
a repository that states none produces a record that *looks* right and is wrong — which is
strictly worse than an empty one, because the UI renders an absent licence honestly as "licence
not stated" and there is no rendering for "invented".

So the registry does not rely on asking nicely. A write may only name a vocabulary term the
registry actually holds, enforced in one place for both REST and MCP, and the refusal explains
how to recover — search, adopt, or mint. Guessing fails loudly rather than quietly.

The corollary for anyone writing an agent against this API: **omit a field you cannot confirm**.
An absent field is rendered honestly. A plausible wrong one is undetectable.

See [Artifact types and topics](../vocabulary/terms.md).

[llmstxt.org]: https://llmstxt.org
