"""Frood as Hermes Agent's memory provider (docs/28 §3).

Before a turn, what Frood knows about the person's message goes into the context, with links
and a "not verified" mark where no person has checked it. Hermes gets two tools: search Frood
and remember something in it. What Hermes writes to its own memory goes to Frood too, as a
note marked "from Hermes memory, not verified". Whole turns are never written: Frood takes
what is worth remembering, not the conversation.

The token is a Frood personal token that can read, capture notes and propose, but not decide:
`frood token create --email you@example.com --label hermes
--scopes knowledge:read,knowledge:capture,knowledge:propose`.

Standard library only, so it installs with nothing else.
"""

from __future__ import annotations

import json
import logging
import os
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any, Optional

from agent.memory_provider import MemoryProvider, spawn_context_thread
from agent.secret_scope import get_secret

logger = logging.getLogger(__name__)

DEFAULT_URL = "http://127.0.0.1:8080"
# Recall waits this long for the search started when the turn began, then goes on without it.
PREFETCH_WAIT_SECS = 3.0
REQUEST_TIMEOUT_SECS = 10.0
RECALL_LIMIT = 8
# After this many failures in a row Frood is left alone for a while: a stopped laptop must not
# slow every turn.
BREAKER_THRESHOLD, BREAKER_COOLDOWN_SECS = 3, 120

SEARCH_SCHEMA = {
    "name": "frood_search",
    "description": (
        "Search Frood, the person's memory: decisions, commitments, rules, facts and statuses, "
        "each with its sources. Use it before answering about the person's work or life."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": "What to look for, in any language."},
            "limit": {"type": "integer", "description": "How many results, 1 to 20 (default 8)."},
        },
        "required": ["query"],
    },
}

REMEMBER_SCHEMA = {
    "name": "frood_remember",
    "description": (
        "Remember something in Frood: a short note in the person's words. Frood reads it into "
        "knowledge marked 'from Hermes memory, not verified'; where it contradicts what is known, "
        "the person decides."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "content": {"type": "string", "description": "What to remember, as a short note."},
        },
        "required": ["content"],
    },
}


class FroodError(Exception):
    """Frood could not be reached or refused the request."""


class Frood:
    """The few Frood calls the provider makes."""

    def __init__(self, url: str, token: str, timeout: float = REQUEST_TIMEOUT_SECS) -> None:
        self.url = url.rstrip("/")
        self.token = token
        self.timeout = timeout

    def _call(self, method: str, path: str, body: Optional[dict[str, Any]] = None,
              timeout: Optional[float] = None) -> Any:
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request(
            self.url + path,
            data=data,
            method=method,
            headers={
                "Authorization": f"Bearer {self.token}",
                "Content-Type": "application/json",
                "Accept": "application/json",
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=timeout or self.timeout) as response:
                return json.loads(response.read() or b"null")
        except urllib.error.HTTPError as err:
            try:
                detail = json.loads(err.read()).get("detail")
            except Exception:  # noqa: BLE001 - any unreadable body is just "no detail"
                detail = None
            raise FroodError(detail or f"Frood answered {err.code}") from err
        except (urllib.error.URLError, TimeoutError, OSError) as err:
            raise FroodError(f"Frood at {self.url} is not reachable: {err}") from err

    def search(self, query: str, limit: int = RECALL_LIMIT,
               timeout: Optional[float] = None) -> list[dict[str, Any]]:
        params = urllib.parse.urlencode({"q": query, "limit": limit, "status": "active"})
        hits = self._call("GET", f"/v1/search?{params}", timeout=timeout)
        return hits if isinstance(hits, list) else []

    def remember(self, content: str, origin: str = "Hermes") -> dict[str, Any]:
        return self._call("POST", "/v1/sources", {
            "kind": "note",
            "origin": "capture",
            "content": content,
            "metadata": {"imported_from": origin},
        })


def line(hit: dict[str, Any]) -> str:
    """One piece of knowledge as the model reads it: the text, its kind, the mark, the id."""
    marks = [hit.get("kind") or "knowledge"]
    if hit.get("unverified"):
        marks.append("not verified")
    title = hit.get("title")
    text = hit.get("text") or ""
    shown = f"{title}: {text}" if title and title not in text else text
    return f"- {shown} ({', '.join(marks)}) [frood:{hit.get('assertion_id', '?')}]"


def recall_block(hits: list[dict[str, Any]]) -> str:
    if not hits:
        return ""
    return "## What Frood knows\n" + "\n".join(line(h) for h in hits)


class FroodMemoryProvider(MemoryProvider):
    """Frood behind Hermes' memory: recall before a turn, tools, Hermes' own writes mirrored."""

    def __init__(self) -> None:
        self._frood: Optional[Frood] = None
        self._writes = True
        self._lock = threading.Lock()
        self._recall_query: Optional[str] = None
        self._recall_thread: Optional[threading.Thread] = None
        self._recall_text = ""
        self._writers: list[threading.Thread] = []
        self._failures = 0
        self._paused_until = 0.0

    @property
    def name(self) -> str:
        return "frood"

    # -- setup ---------------------------------------------------------------------------------

    def is_available(self) -> bool:
        return bool(get_secret("FROOD_TOKEN"))

    def unavailable_reason(self) -> str:
        return "FROOD_TOKEN is not set: `hermes memory setup`, or `frood token create` on the Frood side"

    def get_config_schema(self) -> list[dict[str, Any]]:
        return [
            {
                "key": "url",
                "description": "Frood's address",
                "default": DEFAULT_URL,
                "required": True,
            },
            {
                "key": "token",
                "description": "A Frood token for Hermes (read, capture, propose)",
                "secret": True,
                "required": True,
                "env_var": "FROOD_TOKEN",
                "url": "https://github.com/froodai/frood/tree/main/integrations/hermes",
            },
        ]

    def save_config(self, values: dict[str, Any], hermes_home: str) -> None:
        path = Path(hermes_home) / "frood.json"
        current = _read_json(path)
        current.update({k: v for k, v in values.items() if k != "token"})
        path.write_text(json.dumps(current, indent=2))

    def initialize(self, session_id: str, **kwargs: Any) -> None:
        home = Path(kwargs.get("hermes_home") or os.path.expanduser("~/.hermes"))
        url = _read_json(home / "frood.json").get("url") or os.environ.get("FROOD_URL") or DEFAULT_URL
        self._frood = Frood(url, get_secret("FROOD_TOKEN") or "")
        # Subagents, cron runs and flushes read but do not write: the person's own agent does.
        self._writes = kwargs.get("agent_context", "primary") == "primary"

    def system_prompt_block(self) -> str:
        return (
            "Frood is the person's long-term memory. Before a turn, what it knows about the "
            "message appears under 'What Frood knows', each item with its id [frood:…]; items "
            "marked 'not verified' were taken from an assistant's memory or an agent's finding "
            "and no person has checked them, so say so when you rely on one. Use frood_search to "
            "look further and frood_remember to save something worth keeping."
        )

    # -- recall --------------------------------------------------------------------------------

    def on_turn_start(self, turn_number: int, message: str, **kwargs: Any) -> None:
        self._start_recall(message)

    def queue_prefetch(self, query: str, *, session_id: str = "") -> None:
        self._start_recall(query)

    def prefetch(self, query: str, *, session_id: str = "") -> str:
        with self._lock:
            thread = self._recall_thread if self._recall_query == query else None
        if thread is None:
            self._start_recall(query)
            with self._lock:
                thread = self._recall_thread
        if thread is not None:
            thread.join(timeout=PREFETCH_WAIT_SECS)
        with self._lock:
            return self._recall_text if self._recall_query == query else ""

    def _start_recall(self, query: str) -> None:
        query = (query or "").strip()
        if not query or self._frood is None or self._paused():
            return
        with self._lock:
            if self._recall_query == query and self._recall_thread is not None:
                return
            self._recall_query = query
            self._recall_text = ""
            thread = spawn_context_thread(self._recall, name="frood-recall", args=(query,))
            self._recall_thread = thread
        thread.start()

    def _recall(self, query: str) -> None:
        try:
            text = recall_block(self._frood.search(query))
            self._ok()
        except FroodError as err:
            self._failed(err)
            text = ""
        with self._lock:
            if self._recall_query == query:
                self._recall_text = text

    # -- tools ---------------------------------------------------------------------------------

    def get_tool_schemas(self) -> list[dict[str, Any]]:
        return [SEARCH_SCHEMA, REMEMBER_SCHEMA]

    def handle_tool_call(self, tool_name: str, args: dict[str, Any], **kwargs: Any) -> str:
        if self._frood is None:
            return json.dumps({"error": "Frood is not set up"})
        try:
            if tool_name == "frood_search":
                query = str(args.get("query") or "").strip()
                if not query:
                    return json.dumps({"error": "query is required"})
                limit = max(1, min(int(args.get("limit") or RECALL_LIMIT), 20))
                hits = self._frood.search(query, limit)
                return json.dumps({
                    "results": [
                        {
                            "id": h.get("assertion_id"),
                            "kind": h.get("kind"),
                            "title": h.get("title"),
                            "text": h.get("text"),
                            "not_verified": bool(h.get("unverified")),
                            "valid_from": h.get("valid_from"),
                            "sources": h.get("evidence", []),
                        }
                        for h in hits
                    ]
                }, ensure_ascii=False)
            if tool_name == "frood_remember":
                content = str(args.get("content") or "").strip()
                if not content:
                    return json.dumps({"error": "content is required"})
                saved = self._frood.remember(content)
                source = (saved or {}).get("source") or {}
                return json.dumps({
                    "saved": True,
                    "source": source.get("id"),
                    "note": "Frood reads it into knowledge marked 'from Hermes memory, not verified'.",
                })
        except FroodError as err:
            return json.dumps({"error": str(err)})
        return json.dumps({"error": f"unknown tool {tool_name}"})

    # -- Hermes' own memory --------------------------------------------------------------------

    def on_memory_write(self, action: str, target: str, content: str,
                        metadata: Optional[dict[str, Any]] = None) -> None:
        # What Hermes keeps about the person or the work, Frood keeps too. A removal stays in
        # Hermes: Frood keeps history, and taking knowledge back is the person's decision.
        if action not in ("add", "replace") or not (content or "").strip():
            return
        if not self._writes or self._frood is None:
            return
        thread = spawn_context_thread(self._mirror, name="frood-mirror", args=(content.strip(),))
        self._writers = [t for t in self._writers if t.is_alive()] + [thread]
        thread.start()

    def _mirror(self, content: str) -> None:
        try:
            self._frood.remember(content)
            self._ok()
        except FroodError as err:
            self._failed(err)

    # -- health --------------------------------------------------------------------------------

    def _paused(self) -> bool:
        return time.monotonic() < self._paused_until

    def _ok(self) -> None:
        self._failures = 0

    def _failed(self, err: Exception) -> None:
        self._failures += 1
        logger.warning("Frood: %s", err)
        if self._failures >= BREAKER_THRESHOLD:
            self._paused_until = time.monotonic() + BREAKER_COOLDOWN_SECS
            self._failures = 0

    def shutdown(self) -> None:
        for thread in [self._recall_thread, *self._writers]:
            if thread is not None and thread.is_alive():
                thread.join(timeout=5.0)


def _read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text())
        return value if isinstance(value, dict) else {}
    except (OSError, ValueError):
        return {}


def register(ctx) -> None:
    """Called by Hermes' memory plugin discovery: a new instance per load."""
    ctx.register_memory_provider(FroodMemoryProvider())
