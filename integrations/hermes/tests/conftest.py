"""The provider outside Hermes: Hermes' modules as small stand-ins, a fake Frood over HTTP."""

from __future__ import annotations

import importlib.util
import json
import sys
import threading
import types
from abc import ABC, abstractmethod
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

import pytest


def _hermes_stand_ins(secrets: dict[str, str]) -> None:
    """`agent.memory_provider` and `agent.secret_scope` as Hermes shapes them."""
    memory_provider = types.ModuleType("agent.memory_provider")

    class MemoryProvider(ABC):
        @property
        @abstractmethod
        def name(self) -> str: ...

        @abstractmethod
        def is_available(self) -> bool: ...

        @abstractmethod
        def initialize(self, session_id: str, **kwargs) -> None: ...

        @abstractmethod
        def get_tool_schemas(self) -> list: ...

        # Defaults as in Hermes' agent/memory_provider.py.
        def unavailable_reason(self) -> str:
            return ""

        def system_prompt_block(self) -> str:
            return ""

        def prefetch(self, query, *, session_id=""):
            return ""

        def queue_prefetch(self, query, *, session_id=""):
            pass

        def sync_turn(self, user_content, assistant_content, *, session_id="", messages=None,
                      turn_author=None):
            pass

        def handle_tool_call(self, tool_name, args, **kwargs):
            raise NotImplementedError(tool_name)

        def shutdown(self):
            pass

        def on_turn_start(self, turn_number, message, **kwargs):
            pass

        def on_memory_write(self, action, target, content, metadata=None):
            pass

        def get_config_schema(self):
            return []

        def save_config(self, values, hermes_home):
            pass

    def spawn_context_thread(target, *, name, daemon=True, args=(), kwargs=None):
        return threading.Thread(target=target, args=args, kwargs=kwargs, name=name, daemon=daemon)

    memory_provider.MemoryProvider = MemoryProvider
    memory_provider.spawn_context_thread = spawn_context_thread
    secret_scope = types.ModuleType("agent.secret_scope")
    secret_scope.get_secret = secrets.get
    agent = types.ModuleType("agent")
    sys.modules.update({
        "agent": agent,
        "agent.memory_provider": memory_provider,
        "agent.secret_scope": secret_scope,
    })


class FakeFrood:
    """Answers /v1/search and /v1/sources the way Frood does, and remembers what it was sent."""

    def __init__(self) -> None:
        self.knowledge: list[dict] = []
        self.notes: list[dict] = []
        self.searches: list[dict] = []
        self.token = "frood_pat_test"
        self.down = False
        fake = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):  # quiet
                pass

            def _answer(self, status: int, body) -> None:
                data = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def _authorized(self) -> bool:
                if fake.down:
                    self._answer(503, {"detail": "down"})
                    return False
                if self.headers.get("Authorization") != f"Bearer {fake.token}":
                    self._answer(401, {"detail": "the token is not known"})
                    return False
                return True

            def do_GET(self):
                url = urlparse(self.path)
                if url.path != "/v1/search":
                    return self._answer(404, {"detail": "not found"})
                if not self._authorized():
                    return
                query = {k: v[0] for k, v in parse_qs(url.query).items()}
                fake.searches.append(query)
                words = query.get("q", "").lower().split()
                hits = [k for k in fake.knowledge if any(w in k["text"].lower() for w in words)]
                self._answer(200, hits[: int(query.get("limit", 20))])

            def do_POST(self):
                if urlparse(self.path).path != "/v1/sources":
                    return self._answer(404, {"detail": "not found"})
                if not self._authorized():
                    return
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                fake.notes.append(body)
                self._answer(201, {"source": {"id": f"src-{len(fake.notes)}", "evidence": [{"id": "ev"}]}})

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self) -> None:
        self.server.shutdown()


@pytest.fixture
def frood():
    fake = FakeFrood()
    yield fake
    fake.close()


@pytest.fixture
def provider_module(frood, monkeypatch):
    """The plugin loaded fresh, with Hermes' stand-ins and the fake Frood's token as the secret."""
    _hermes_stand_ins({"FROOD_TOKEN": frood.token})
    monkeypatch.setenv("FROOD_URL", frood.url)
    path = Path(__file__).resolve().parent.parent / "__init__.py"
    spec = importlib.util.spec_from_file_location("frood_hermes", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module
