"""Contract tests for hard chat deletion + embedding purge.

Run: python test_chat_delete.py   (exit 0 = pass)
"""
import os
import sqlite3
import tempfile

import config
import dialog_store as ds
import rag

tmp = tempfile.mkdtemp(prefix="gmlab_del_")
db = os.path.join(tmp, "dialogs.sqlite3")

# Keep the DB-level test isolated from any real RAG cache.
config.RAG_ENABLED = False

store = ds.DialogStore(db, lambda: None)
guest = "shared"

c1 = store.create_chat(guest, title="Чат 1", activate=True)
c2 = store.create_chat(guest, title="Чат 2", activate=True)  # c2 is active now
assert len(store.list_chats(guest)) == 2
assert store.active_chat_id(guest) == c2.chat_id

# delete the ACTIVE chat -> row gone, active pointer moves to the survivor
res = store.delete_chat(guest, c2.chat_id)
assert res["deleted"] is True, res
ids = [c["id"] for c in store.list_chats(guest)]
assert ids == [c1.chat_id], ids
assert store.active_chat_id(guest) == c1.chat_id

con = sqlite3.connect(db)
assert con.execute("SELECT COUNT(*) FROM dialog_chats WHERE chat_id=?", (c2.chat_id,)).fetchone()[0] == 0
con.close()

# deleting unknown / empty ids is a no-op
assert store.delete_chat(guest, c2.chat_id)["deleted"] is False  # already gone
assert store.delete_chat(guest, "")["deleted"] is False

# delete the LAST chat -> get_active transparently creates a fresh one
assert store.delete_chat(guest, c1.chat_id)["deleted"] is True
assert store.list_chats(guest) == []
fresh = store.get_active(guest)
assert fresh.chat_id not in (c1.chat_id, c2.chat_id)
assert [c["id"] for c in store.list_chats(guest)] == [fresh.chat_id]
print("[OK] delete_chat: row removed from DB, active pointer fixed, fresh chat when empty")

# --- embedding cache: delete across models, keep unrelated ---
cache = rag.EmbeddingCache(os.path.join(tmp, "emb.sqlite3"))
cache.put_many("m1", [("alpha text", [0.1, 0.2]), ("beta text", [0.3, 0.4])])
cache.put_many("m2", [("alpha text", [0.5, 0.6])])  # same text, different model
removed = cache.delete_by_text_hashes([rag._sha("alpha text")])
assert removed == 2, removed  # both models' rows for 'alpha text'
assert not cache.get_many("m1", ["alpha text"])
assert cache.get_many("m1", ["beta text"])
print("[OK] EmbeddingCache.delete_by_text_hashes drops across models, keeps unrelated")

# --- purge helper via config path (matches embed() strip+hash) ---
config.RAG_ENABLED = True
config.RAG_CACHE_PATH = os.path.join(tmp, "purge.sqlite3")
pc = rag.EmbeddingCache(config.RAG_CACHE_PATH)
pc.put_many(config.RAG_EMBEDDINGS_MODEL, [("hello world", [1.0, 0.0])])
assert rag.purge_embeddings_for_texts(["  hello world  "]) == 1  # strip-normalized
assert not rag.EmbeddingCache(config.RAG_CACHE_PATH).get_many(config.RAG_EMBEDDINGS_MODEL, ["hello world"])
assert rag.purge_embeddings_for_texts([]) == 0
print("[OK] purge_embeddings_for_texts removes via config path, no-ops on empty")

print("ALL OK")
