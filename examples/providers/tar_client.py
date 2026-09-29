"""A minimal client for a service provider's deployment: authenticate, announce, advertise.

Standard library only, so it drops into any job without a dependency. Copy it, or import it.
Configuration comes from the environment, the same variables the shell examples use:

    TAR_URL             the registry, e.g. https://registry.example.org
    TAR_TOKEN_URL       your identity provider's token endpoint
    TAR_CLIENT_ID       this deployment's client id
    TAR_CLIENT_SECRET   its secret
    TAR_TOKEN           instead of the three above: a registry API token

Usage:

    python tar_client.py whoami
    python tar_client.py announce          # PUT /api/v1/instances/self from TAR_* settings
    python tar_client.py advertise-example # advertise one produced artifact
"""

from __future__ import annotations

import base64
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request


class RegistryError(RuntimeError):
    """A non-2xx answer. `problem` is the registry's RFC 9457 body when it sent one."""

    def __init__(self, status: int, problem: dict | str):
        self.status, self.problem = status, problem
        detail = problem.get("detail") if isinstance(problem, dict) else problem
        super().__init__(f"HTTP {status}: {detail}")


class Registry:
    def __init__(self, url: str | None = None):
        self.url = (url or os.environ["TAR_URL"]).rstrip("/")
        self._token, self._expires = os.environ.get("TAR_TOKEN"), float("inf")
        if not self._token:
            self._expires = 0.0  # fetched on first use

    def _bearer(self) -> str:
        # A client-credentials token is short-lived; fetch a new one a minute before it lapses
        # rather than handling a 401 halfway through a job.
        if time.time() > self._expires - 60:
            form = urllib.parse.urlencode({"grant_type": "client_credentials"}).encode()
            req = urllib.request.Request(os.environ["TAR_TOKEN_URL"], data=form, method="POST")
            creds = f"{os.environ['TAR_CLIENT_ID']}:{os.environ['TAR_CLIENT_SECRET']}"
            req.add_header("Authorization", "Basic " + base64.b64encode(creds.encode()).decode())
            with urllib.request.urlopen(req, timeout=30) as r:
                body = json.load(r)
            self._token, self._expires = body["access_token"], time.time() + body.get("expires_in", 300)
        return self._token

    def request(self, method: str, path: str, body: dict | None = None, retries: int = 3):
        data = json.dumps(body).encode() if body is not None else None
        for attempt in range(retries + 1):
            req = urllib.request.Request(self.url + path, data=data, method=method)
            req.add_header("Authorization", f"Bearer {self._bearer()}")
            req.add_header("Accept", "application/json")
            if data is not None:
                req.add_header("Content-Type", "application/json")
            try:
                with urllib.request.urlopen(req, timeout=60) as r:
                    raw = r.read()
                    return json.loads(raw) if raw else None
            except urllib.error.HTTPError as e:
                raw = e.read().decode(errors="replace")
                try:
                    problem = json.loads(raw)
                except ValueError:
                    problem = raw
                # Over a rate limit: wait as long as the registry says, then try again.
                if e.code == 429 and attempt < retries:
                    time.sleep(int(e.headers.get("Retry-After", "5")))
                    continue
                raise RegistryError(e.code, problem) from None

    def whoami(self) -> dict:
        return self.request("GET", "/api/v1/whoami")

    def announce(self, record: dict) -> dict:
        """Create or update this deployment's own record. Safe to call on every start."""
        return self.request("PUT", "/api/v1/instances/self", record)

    def advertise(self, role: str, run: dict, artifacts: list[dict]) -> dict:
        """`role` is "produced" or "consumed". Idempotent on the run's and artifacts' external keys."""
        return self.request("POST", f"/api/v1/advertise/{role}", {"run": run, "artifacts": artifacts})


def _main(argv: list[str]) -> int:
    reg = Registry()
    cmd = argv[1] if len(argv) > 1 else "whoami"
    if cmd == "whoami":
        print(json.dumps(reg.whoami(), indent=2))
    elif cmd == "announce":
        record = {"label": os.environ["TAR_DEPLOYMENT_LABEL"], "endpoint_url": os.environ["TAR_ENDPOINT_URL"]}
        if os.environ.get("TAR_HEALTH_URL"):
            record["health_endpoint"] = os.environ["TAR_HEALTH_URL"]
        if os.environ.get("TAR_RELEASE_ID"):
            record["release"] = os.environ["TAR_RELEASE_ID"]
        print(json.dumps(reg.announce(record), indent=2))
    elif cmd == "advertise-example":
        job = os.environ.get("JOB_ID", "local-1")
        result = reg.advertise(
            "produced",
            run={"external_key": f"example/{job}", "status": "success"},
            artifacts=[
                {
                    "external_key": f"example/{job}/report",
                    "title": f"Example report for job {job}",
                    "conforms_to": os.environ["TAR_ARTIFACT_TYPE"],
                    "distributions": [
                        {"download_url": f"https://files.example.org/{job}/report.ttl", "media_type": "text/turtle"}
                    ],
                }
            ],
        )
        print(json.dumps(result, indent=2))
    else:
        print(__doc__, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    try:
        sys.exit(_main(sys.argv))
    except RegistryError as e:
        print(e, file=sys.stderr)
        sys.exit(1)
