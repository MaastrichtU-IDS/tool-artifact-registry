"""Shared plumbing for the two simulated applications.

Standard library only, on purpose. These programs exist to show the *HTTP contract* a real
deployment has to satisfy — advertise what you made, subscribe to what you care about, drain
the queue, acknowledge what you finished. A dependency to install would be a distraction, and
would also hide how little a tool needs in order to participate.

Nothing here reaches into the registry's database or graph. Everything is `/api/v1/...`.
"""

from __future__ import annotations

import hashlib
import http.server
import json
import os
import socketserver
import sys
import threading
import urllib.error
import urllib.parse
import urllib.request

# ------------------------------------------------------------------ output


_COLOURS = {"sulo": "\033[35m", "onto": "\033[36m"}
_RESET = "\033[0m"
_DIM = "\033[2m"


class Log:
    """A tagged, line-buffered logger so two processes can share one terminal."""

    def __init__(self, tag: str) -> None:
        self.tag = tag
        self.colour = _COLOURS.get(tag, "")
        self.use_colour = sys.stdout.isatty() and os.environ.get("NO_COLOR") is None

    def _emit(self, text: str, dim: bool = False) -> None:
        tag = f"[{self.tag}]"
        if self.use_colour:
            tag = f"{self.colour}{tag}{_RESET}"
            if dim:
                text = f"{_DIM}{text}{_RESET}"
        print(f"  {tag} {text}", flush=True)

    def say(self, text: str) -> None:
        self._emit(text)

    def detail(self, text: str) -> None:
        self._emit(f"    {text}", dim=True)

    def warn(self, text: str) -> None:
        self._emit(f"! {text}")


# ------------------------------------------------------------------ registry client


class RegistryError(RuntimeError):
    def __init__(self, method: str, path: str, status: int, body: str) -> None:
        self.status = status
        self.body = body
        detail = body
        try:
            parsed = json.loads(body)
            detail = f"{parsed.get('title', '')} — {parsed.get('detail', '')}".strip(" —")
            # A 422 carries the engine's own sh:ValidationReport. It is the most useful error
            # this API produces, so never swallow it.
            if parsed.get("report"):
                detail += "\n--- SHACL validation report ---\n" + parsed["report"]
        except Exception:
            pass
        super().__init__(f"{method} {path} -> {status}: {detail}")


class Registry:
    """One deployment's view of the registry: a base URL and its own credential.

    The credential is what decides *which Instance* the registry attributes a write to
    (spec §8.3) — the payload never names it, and could not be believed if it did.
    """

    def __init__(self, base: str, token: str | None = None, timeout: float = 15.0) -> None:
        self.base = base.rstrip("/")
        self.token = token
        self.timeout = timeout

    def _request(self, method: str, path: str, body: dict | None = None, params: dict | None = None):
        url = self.base + path
        if params:
            url += "?" + urllib.parse.urlencode({k: v for k, v in params.items() if v is not None})
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(url, data=data, method=method)
        if data is not None:
            req.add_header("content-type", "application/json")
        if self.token:
            req.add_header("authorization", f"Bearer {self.token}")
        req.add_header("accept", "application/json")
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                raw = resp.read()
        except urllib.error.HTTPError as e:
            raise RegistryError(method, path, e.code, e.read().decode("utf-8", "replace")) from None
        except urllib.error.URLError as e:
            raise RuntimeError(f"{method} {path}: cannot reach the registry at {self.base}: {e.reason}") from None
        if not raw:
            return None
        return json.loads(raw)

    def get(self, path: str, **params):
        return self._request("GET", path, params=params or None)

    def post(self, path: str, body: dict):
        return self._request("POST", path, body=body)

    def patch(self, path: str, body: dict):
        return self._request("PATCH", path, body=body)


def announce_endpoint(reg: Registry, instance_id: str, endpoint_url: str, description: str | None = None) -> None:
    """Record where this deployment is answering, on its own Instance record.

    A deployment may maintain its own record — the registry's rule is `instance_iri == mine, or
    be a curator`. So a process that binds a port at start-up can say where it ended up, which
    is the only way an endpoint that is not known in advance ever gets recorded.

    **Read-modify-write, deliberately.** `PATCH /api/v1/instances/{id}` is a *replace*, not a
    merge: it rebuilds the record from the body, so a body carrying only `endpoint_url` would
    silently drop the operator, the jurisdiction, the availability and the credential binding.
    Hence the GET first, and hence sending the whole record back.
    """
    current = reg.get("/api/v1/instances/%s" % instance_id)
    body = {"label": current["label"], "endpoint_url": endpoint_url}
    if description:
        body["endpoint_description"] = description
    for key in ("software", "release", "availability", "jurisdiction", "description",
                "oidc_client_id", "oidc_issuer"):
        if current.get(key):
            body[key] = current[key]
    if current.get("allowed_scopes"):
        body["allowed_scopes"] = current["allowed_scopes"]
    if current.get("operator"):
        op = current["operator"]
        body["operator"] = {k: op[k] for k in ("name", "kind", "identifier", "email", "homepage") if op.get(k)}
    # Only when this deployment declared its own narrowing. A capability inherited from the
    # Software record must not be copied down here — that would turn "what this tool can do"
    # into "what this one deployment claims to do", which is a different statement.
    cap = current.get("capability")
    if cap and cap.get("declared_at") == "instance":
        body["capability"] = {
            "produces": [t["iri"] for t in cap.get("produces", [])],
            "consumes": [t["iri"] for t in cap.get("consumes", [])],
        }
    reg.patch("/api/v1/instances/%s" % instance_id, body)


def fetch_bytes(url: str, timeout: float = 30.0) -> bytes:
    """Fetch a distribution's bytes. A subscriber does this itself: the registry stores no
    payloads (spec D1), it only says where they are and on what terms."""
    with urllib.request.urlopen(url, timeout=timeout) as resp:
        return resp.read()


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 16), b""):
            h.update(chunk)
    return h.hexdigest()


# ------------------------------------------------------------------ file serving


class _QuietHandler(http.server.SimpleHTTPRequestHandler):
    """Serve the deployment's own output directory, with honest content types.

    A registry record that advertises `media_type: text/turtle` and then serves
    `application/octet-stream` is a small lie that costs a downstream tool a content sniff.
    """

    extensions_map = {
        **http.server.SimpleHTTPRequestHandler.extensions_map,
        ".ttl": "text/turtle",
        ".owl": "application/rdf+xml",
        ".json": "application/json",
        ".jsonld": "application/ld+json",
        ".html": "text/html; charset=utf-8",
        "": "application/octet-stream",
    }

    def log_message(self, fmt, *args):  # noqa: D102 - the demo prints its own story
        pass


class _Server(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True


def serve_directory(directory: str, host: str = "127.0.0.1") -> tuple[str, _Server]:
    """Start a background HTTP server on a free port and return `(base_url, server)`.

    Port 0 means the kernel picks a free port, so two demo runs never collide and neither
    can steal a port something else is using.
    """
    os.makedirs(directory, exist_ok=True)
    handler = lambda *a, **kw: _QuietHandler(*a, directory=directory, **kw)  # noqa: E731
    httpd = _Server((host, 0), handler)
    port = httpd.server_address[1]
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return f"http://{host}:{port}", httpd


# ------------------------------------------------------------------ misc


def load_config(path: str) -> dict:
    """The deployment's own configuration: where the registry is, which Instance it is, its
    credential, and which artifact-type IRIs its operator told it to use.

    This is what an operator provisions once. Note what is *not* in it: the root token, the
    other application's credential, or any knowledge of the other application at all.
    """
    with open(path) as fh:
        return json.load(fh)


def write_pid(path: str | None) -> None:
    if path:
        with open(path, "w") as fh:
            fh.write(str(os.getpid()))


def write_marker(path: str | None, payload: dict | None = None) -> None:
    if not path:
        return
    tmp = path + ".tmp"
    with open(tmp, "w") as fh:
        json.dump(payload or {"ok": True}, fh)
    os.replace(tmp, path)
