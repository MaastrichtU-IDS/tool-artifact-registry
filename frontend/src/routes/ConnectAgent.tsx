import { useState } from 'react'
import { useSession } from '../lib/session'
import { CopyBlock, CopyField, Skeleton } from '../components/common'

/**
 * How to point an agent at this registry.
 *
 * Every snippet is built from the registry's own base IRI rather than a placeholder, because
 * the single most common way this goes wrong is someone copying `your-registry.example` out of
 * a document and wondering why nothing connects.
 */

type ClientId = 'claude-code' | 'claude-desktop' | 'editors' | 'sdk' | 'raw'

const CLIENTS: { id: ClientId; name: string; what: string }[] = [
  { id: 'claude-code', name: 'Claude Code', what: 'The CLI, in a terminal' },
  { id: 'claude-desktop', name: 'Claude Desktop & claude.ai', what: 'Added as a connector' },
  { id: 'editors', name: 'Cursor, VS Code, Windsurf', what: 'An mcp.json file' },
  { id: 'sdk', name: 'Your own agent', what: 'Any MCP client library' },
  { id: 'raw', name: 'curl', what: 'To check it works at all' },
]

export default function ConnectAgent() {
  const { registry, loading } = useSession()
  const [client, setClient] = useState<ClientId>('claude-code')

  if (loading) return <Skeleton rows={6} />

  // The page is served from the registry, so its own origin is the honest default even before
  // discovery has answered.
  const base = registry?.base_iri ?? window.location.origin
  const url = `${base}/mcp`
  const oidc = registry?.auth?.oidc
  const name = 'tar'

  const jsonConfig = JSON.stringify(
    { mcpServers: { [name]: { type: 'http', url } } },
    null,
    2,
  )

  return (
    <>
      <div className="page-header">
        <h1>Connect an agent</h1>
        <p className="tagline">
          This registry hosts its own MCP server. There is nothing to install — an agent
          connects to a URL and can then search the catalogue, look up vocabulary terms and
          register what it knows.
        </p>
      </div>

      <section className="card">
        <h2>The server</h2>
        <CopyField value={url} label="MCP endpoint" />
        <p className="hint">
          Streamable HTTP. The same endpoint serves every client below; only the way you tell
          the client about it differs.
        </p>
      </section>

      <section className="card">
        <h2>Signing in</h2>
        {oidc?.enabled ? (
          <>
            <p style={{ marginTop: 0 }}>
              A client that speaks OAuth needs no configuration: it discovers{' '}
              <code>{base}/.well-known/oauth-protected-resource</code>, is sent to{' '}
              <code>{oidc.issuer}</code>, and a browser window opens for you to sign in. This is
              the option to prefer — nothing long-lived is stored in a config file.
            </p>
            <p>
              For a client that does not, or for a script, use a registry API token instead:
              mint one from a deployment's <strong>Credentials</strong> page, or from a
              software's <strong>Auto-registration</strong> page, and send it as a bearer token.
            </p>
          </>
        ) : (
          <>
            <p style={{ marginTop: 0 }}>
              This registry has no identity provider configured, so use a registry API token:
              mint one from a deployment's <strong>Credentials</strong> page and send it as a
              bearer token.
            </p>
            <p className="hint">
              Reads that are already public need no credential at all — an agent can connect
              anonymously and search. It will simply be offered fewer tools.
            </p>
          </>
        )}
        <p className="hint">
          Whatever the credential, an agent can never do more through MCP than that credential
          could do through the API: every tool call is checked by the same rules.
        </p>
      </section>

      <section className="card">
        <h2>Adding it</h2>
        <div className="tabs" role="tablist" aria-label="Client" style={{ marginBottom: 14 }}>
          {CLIENTS.map((c) => (
            <button
              key={c.id}
              type="button"
              role="tab"
              aria-selected={client === c.id}
              className={client === c.id ? 'active' : undefined}
              onClick={() => setClient(c.id)}
            >
              {c.name}
            </button>
          ))}
        </div>

        {client === 'claude-code' && (
          <>
            <p style={{ marginTop: 0 }}>Run this in any project:</p>
            <CopyBlock label="the Claude Code command" code={`claude mcp add --transport http ${name} ${url}`} />
            <p>
              Then <code>/mcp</code> inside Claude Code lists the connection and, where the
              registry uses an identity provider, starts the sign-in. Add{' '}
              <code>--scope user</code> to make it available in every project rather than this
              one.
            </p>
            <p className="hint" style={{ marginBottom: 4 }}>With a token instead of OAuth:</p>
            <CopyBlock
              label="the Claude Code command with a token"
              code={`claude mcp add --transport http ${name} ${url} \\
  --header "Authorization: Bearer $TAR_TOKEN"`}
            />
          </>
        )}

        {client === 'claude-desktop' && (
          <>
            <p style={{ marginTop: 0 }}>
              In Claude Desktop or on claude.ai, open <strong>Settings → Connectors</strong>,
              choose <strong>Add custom connector</strong>, and give it this URL:
            </p>
            <CopyField value={url} label="connector URL" />
            <p>
              {oidc?.enabled
                ? 'Sign-in happens in a browser window the first time the agent uses a tool.'
                : 'This registry has no identity provider, so a client that can only do OAuth will connect anonymously and see the read-only tools.'}
            </p>
            <p className="hint">
              Custom connectors are available on paid plans; on a plan without them, use one of
              the other options here.
            </p>
          </>
        )}

        {client === 'editors' && (
          <>
            <p style={{ marginTop: 0 }}>
              Cursor, VS Code and Windsurf all read a JSON file — <code>.cursor/mcp.json</code>{' '}
              or <code>.vscode/mcp.json</code> in the project, or the editor's global
              equivalent. The shape is the same:
            </p>
            <CopyBlock label="the mcp.json config" code={jsonConfig} />
            <p className="hint">
              Some builds spell the key <code>servers</code> rather than{' '}
              <code>mcpServers</code>, and older ones want{' '}
              <code>"type": "streamable-http"</code>. If the editor does not pick it up, check
              its own documentation for which it expects — the URL is the part that matters.
            </p>
          </>
        )}

        {client === 'sdk' && (
          <>
            <p style={{ marginTop: 0 }}>
              Any MCP client library can connect over Streamable HTTP. The endpoint is stateless
              — no session to keep alive, no second connection for a stream — so a plain HTTP
              client is enough if you would rather not take a dependency.
            </p>
            <CopyBlock label="the Python client snippet" code={`from mcp import ClientSession
from mcp.client.streamable_http import streamablehttp_client

async with streamablehttp_client(
    "${url}",
    headers={"Authorization": f"Bearer {token}"},
) as (read, write, _):
    async with ClientSession(read, write) as session:
        await session.initialize()
        tools = await session.list_tools()`} />
            <p className="hint">
              The tools your credential is allowed to use are the ones it returns — the list is
              filtered per credential, so an anonymous client sees only the read-only ones.
            </p>
          </>
        )}

        {client === 'raw' && (
          <>
            <p style={{ marginTop: 0 }}>
              To check the server is reachable and your credential works before involving any
              agent:
            </p>
            <CopyBlock label="the curl command" code={`curl -s -X POST ${url} \\
  -H "Authorization: Bearer $TAR_TOKEN" \\
  -H "content-type: application/json" \\
  -H "accept: application/json, text/event-stream" \\
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'`} />
            <p>
              A list of tools means everything works. A <code>401</code> means the credential
              was rejected — the <code>WWW-Authenticate</code> header on that response names
              where to authenticate.
            </p>
          </>
        )}
      </section>

      <section className="card">
        <h2>What an agent can do once connected</h2>
        <p style={{ marginTop: 0 }}>
          Search the catalogue and read any record; look up controlled terms before citing them;
          register software, releases, capabilities and deployments; and, for a credential bound
          to a deployment, advertise what it produced and consumed.
        </p>
        <p>
          Minting credentials, deleting records, managing peers and raw SPARQL are deliberately
          not exposed as tools. They are absent rather than gated, so there is nothing for an
          agent to talk its way into.
        </p>
        <p className="hint">
          An agent that would rather read pages than call tools can use{' '}
          <a href={`${base}/llms.txt`}>{base}/llms.txt</a>, and append <code>.md</code> to any
          record's URL.
        </p>
      </section>
    </>
  )
}
