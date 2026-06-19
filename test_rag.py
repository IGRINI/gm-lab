"""RAG retrieval smoke tests with deterministic embeddings."""
import os

os.environ.setdefault("GM_BACKEND", "mock")

import rag
import world as world_mod


rag.set_default_engine(rag.RagEngine(rag.HashEmbeddingClient()))

w = world_mod.World()
docs = w.retrieval_documents()

joined = "\n".join(doc.text for doc in docs)
assert "Thieves' Guild killed him" not in joined
assert any(doc.kind == "public_fact" and "Алдрик" in doc.text for doc in docs)
assert any(doc.kind == "npc_whereabouts" and doc.metadata.get("npc_id") == "mareth" for doc in docs)

mareth = w.fact("Где искать капитана Марет?")
payload = mareth.as_tool_payload()
assert payload["status"] == "known"
assert "Марет" in payload["text"] or "страж" in payload["text"]
assert payload.get("sources")
assert any(source["kind"] == "npc_whereabouts" for source in payload["sources"])

w.record_rumor(42, 3, "borin", "Я видел человека в тёмном плаще у лавки Алдрика.", frozenset({"player", "borin"}))
rumor = w.fact("Кто видел тёмный плащ у лавки Алдрика?")
rumor_payload = rumor.as_tool_payload()
assert "Борин сказал" in rumor_payload["text"]
assert any(source["kind"] == "testimony" for source in rumor_payload.get("sources", []))

rag.set_default_engine(None)

print("RAG TESTS PASSED")
