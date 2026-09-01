# Getting started

## From a checkout

A Rust toolchain, and Node for the UI. The container build pins the versions it uses — see
`Dockerfile` — and the build has no other system dependencies beyond a C toolchain and `clang`.

```bash
cargo build --release
cd frontend && npm install && npm run build && cd ..

export TAR_BASE_IRI=http://127.0.0.1:8080
export TAR_ROOT_TOKEN=$(openssl rand -hex 24)
export TAR_DATA_DIR=./data

./target/release/tar seed      # example content, so a fresh install is not an empty page
./target/release/tar serve
```

Open <http://127.0.0.1:8080>. Everything reads anonymously; sign in with the root token to
register or edit.

`TAR_BASE_IRI` is the only universally required setting — the registry cannot mint
dereferenceable identifiers without knowing what it is called. It refuses to start without it,
and refuses to start with a `TAR_ROOT_TOKEN` that is a recognisable placeholder or shorter than
16 characters.

The `Makefile` wraps the same commands: `make build`, `make run`, `make seed`, `make test`.

### About the base IRI

It becomes part of every identifier the registry mints, permanently. Changing it later does not
rewrite the records that already exist, so a registry that is going to be reachable at a real
hostname should be told that hostname before it is seeded, not after.

For a local trial `http://127.0.0.1:8080` is fine, and the identifiers it mints are honestly
local ones.

## The seed

`tar seed` loads a small worked example so that the first page is not empty: **4 pieces of
software, 5 deployments, 12 runs, 19 artifacts, and 16 registry-minted artifact types**,
including one artifact derived from a record at another registry so that the cross-registry
case is visible from the start.

It is example content, not a fixture anything depends on. `--with-runs=false` loads the
catalogue without the run and artifact graph.

## With Docker

```bash
export TAR_ROOT_TOKEN=$(openssl rand -hex 24)
docker compose up --build
```

`TAR_ROOT_TOKEN` has no default and compose will refuse to start without it, which is
deliberate — a bootstrap credential everyone's install shares is not a credential.

One service, one volume. `TAR_BASE_IRI` defaults to `http://localhost:8080`; set it to whatever
the registry will actually be reachable at.

## First steps once it is running

```bash
# What is this registry, and how do I talk to it?
curl http://127.0.0.1:8080/.well-known/tar-registry

# What does my credential actually let me do?
curl -H "Authorization: Bearer $TAR_ROOT_TOKEN" http://127.0.0.1:8080/api/v1/whoami

# Everything in it, as prose
curl http://127.0.0.1:8080/llms.txt

# What can produce a validation report?
curl 'http://127.0.0.1:8080/api/v1/capabilities?produces=http://127.0.0.1:8080/type/shacl-validation-report'
```

Then:

- [Registering software](api/software.md), if you are filling in a catalogue.
- [How a tool authenticates](api/authentication.md) and [Advertising runs and
  artifacts](api/advertising.md), if you are wiring up a tool.
- [Identity provider setup](operations/identity-provider.md), for sign-in.

## Trying sign-in locally

An importable identity-provider realm — three roles, a PKCE public client, a service-account
client, and users with known passwords — lives in `deploy/keycloak/`. See [Identity provider
setup](operations/identity-provider.md), which also covers the audience mapper that is the one
thing that must line up.

## Running the tests

```bash
cargo test                 # unit, end-to-end, MCP and subscription suites
cd frontend && npm test    # component, parsing and screen tests
```
