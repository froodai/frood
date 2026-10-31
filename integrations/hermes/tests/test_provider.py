"""Frood behind Hermes' memory, against a fake Frood: recall, tools, mirrored writes, outages."""

from __future__ import annotations

import json


def started(module, tmp_path, **kwargs):
    provider = module.FroodMemoryProvider()
    provider.initialize("s1", hermes_home=str(tmp_path), platform="cli", **kwargs)
    return provider


def test_it_registers_a_new_provider_each_load(provider_module):
    registered = []

    class Ctx:
        def register_memory_provider(self, provider):
            registered.append(provider)

    provider_module.register(Ctx())
    provider_module.register(Ctx())
    assert [p.name for p in registered] == ["frood", "frood"]
    assert registered[0] is not registered[1]
    assert registered[0].is_available()


def test_recall_before_a_turn_says_what_is_known_and_what_is_not_verified(provider_module, frood, tmp_path):
    frood.knowledge = [
        {"assertion_id": "a1", "kind": "commitment", "title": None, "text": "Release 2.4 on Nov 14"},
        {"assertion_id": "a2", "kind": "fact", "title": "Hosting", "text": "The site runs on Hetzner",
         "unverified": True},
    ]
    provider = started(provider_module, tmp_path)
    provider.on_turn_start(1, "when is the release and where is hosting?")
    block = provider.prefetch("when is the release and where is hosting?")
    assert block.startswith("## What Frood knows\n")
    assert "- Release 2.4 on Nov 14 (commitment) [frood:a1]" in block
    assert "- Hosting: The site runs on Hetzner (fact, not verified) [frood:a2]" in block
    # One search for the turn, only what holds now.
    assert len(frood.searches) == 1
    assert frood.searches[0]["status"] == "active"
    assert provider.prefetch("something else entirely") == ""
    assert "not verified" in provider.system_prompt_block()


def test_the_tools_search_and_remember_as_hermes_memory(provider_module, frood, tmp_path):
    frood.knowledge = [{"assertion_id": "a1", "kind": "decision", "text": "Search on OpenSearch",
                        "evidence": ["e1"], "unverified": True}]
    provider = started(provider_module, tmp_path)
    assert [s["name"] for s in provider.get_tool_schemas()] == ["frood_search", "frood_remember"]
    found = json.loads(provider.handle_tool_call("frood_search", {"query": "opensearch", "limit": 50}))
    assert found["results"] == [{
        "id": "a1", "kind": "decision", "title": None, "text": "Search on OpenSearch",
        "not_verified": True, "valid_from": None, "sources": ["e1"],
    }]
    assert frood.searches[-1]["limit"] == "20"
    saved = json.loads(provider.handle_tool_call("frood_remember", {"content": "Olya leads the release"}))
    assert saved["saved"] is True and saved["source"] == "src-1"
    assert frood.notes == [{
        "kind": "note", "origin": "capture", "content": "Olya leads the release",
        "metadata": {"imported_from": "Hermes"},
    }]
    assert "error" in json.loads(provider.handle_tool_call("frood_remember", {"content": " "}))
    assert "error" in json.loads(provider.handle_tool_call("frood_forget", {}))


def test_hermes_own_memory_is_mirrored_but_not_removals_or_subagents(provider_module, frood, tmp_path):
    provider = started(provider_module, tmp_path)
    provider.on_memory_write("add", "user", "Prefers tea to coffee")
    provider.on_memory_write("replace", "memory", "Works on Frood in the evenings",
                             {"previous_content": "Works on Frood"})
    provider.on_memory_write("remove", "memory", "Works on Frood in the evenings")
    provider.shutdown()
    assert sorted(n["content"] for n in frood.notes) == [
        "Prefers tea to coffee", "Works on Frood in the evenings"]
    assert all(n["metadata"] == {"imported_from": "Hermes"} for n in frood.notes)

    sub = started(provider_module, tmp_path, agent_context="subagent")
    sub.on_memory_write("add", "memory", "a subagent's scratch")
    sub.shutdown()
    assert len(frood.notes) == 2
    # Whole turns are never written.
    provider.sync_turn("hi", "hello")
    assert len(frood.notes) == 2


def test_a_stopped_frood_does_not_slow_turns(provider_module, frood, tmp_path):
    provider = started(provider_module, tmp_path)
    frood.down = True
    for turn in range(3):
        provider.on_turn_start(turn, f"question {turn}")
        assert provider.prefetch(f"question {turn}") == ""
    asked = len(frood.searches)
    provider.on_turn_start(4, "question 4")
    assert provider.prefetch("question 4") == ""
    assert len(frood.searches) == asked, "after three failures Frood is left alone for a while"
    error = json.loads(provider.handle_tool_call("frood_search", {"query": "x"}))
    assert "error" in error


def test_a_wrong_token_is_said_plainly(provider_module, frood, tmp_path):
    frood.token = "another"
    provider = started(provider_module, tmp_path)
    error = json.loads(provider.handle_tool_call("frood_search", {"query": "x"}))
    assert error == {"error": "the token is not known"}


def test_setup_keeps_the_address_and_leaves_the_token_to_the_secrets(provider_module, frood, tmp_path, monkeypatch):
    provider = provider_module.FroodMemoryProvider()
    fields = {f["key"]: f for f in provider.get_config_schema()}
    assert fields["token"]["secret"] is True and fields["token"]["env_var"] == "FROOD_TOKEN"
    provider.save_config({"url": frood.url, "token": "never written"}, str(tmp_path))
    assert json.loads((tmp_path / "frood.json").read_text()) == {"url": frood.url}
    monkeypatch.delenv("FROOD_URL")
    frood.knowledge = [{"assertion_id": "a1", "kind": "fact", "text": "kept address works"}]
    provider.initialize("s1", hermes_home=str(tmp_path))
    assert json.loads(provider.handle_tool_call("frood_search", {"query": "address"}))["results"]
