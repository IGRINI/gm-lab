// External store for the chat timeline.
//
// Server events (the same kinds the old index.html handled) are folded into a
// flat array of message objects. Streaming deltas replace the target message
// with a shallow clone (immutable update) so React.memo on the row detects the
// change and re-renders ONLY that item — other rows keep their reference and
// are skipped. Flushes are coalesced with rAF so a burst of tokens produces at
// most one render per frame.

export function createTimeline() {
  let arr = [];
  let pub = arr; // published snapshot for useSyncExternalStore
  let liveIdx = new Map(); // "sid|type" -> index in arr (streaming targets)
  let nextId = 1;
  const listeners = new Set();
  let scheduled = false;

  const notify = () => {
    pub = arr.slice();
    listeners.forEach((l) => l());
  };
  const schedule = () => {
    if (scheduled) return;
    scheduled = true;
    requestAnimationFrame(() => {
      scheduled = false;
      notify();
    });
  };

  const push = (msg) => {
    msg.id = nextId++;
    arr.push(msg);
    return arr.length - 1;
  };
  const ensureIdx = (sid, type, make) => {
    const k = sid + "|" + type;
    let idx = liveIdx.get(k);
    if (idx == null) {
      idx = push(make());
      liveIdx.set(k, idx);
    }
    return idx;
  };
  const idxOf = (sid, type) => liveIdx.get(sid + "|" + type);
  const update = (idx, patch) => {
    arr[idx] = { ...arr[idx], ...patch };
  };

  function applyEvent(ev) {
    const k = ev.kind;
    const a = ev.agent;
    const d = ev.data;
    const sid = ev.sid;

    if (k === "player") {
      push({ type: "player", text: d });
    } else if (k === "delta") {
      if (d.channel === "gm_thinking") {
        const i = ensureIdx(sid, "gm_think", () => ({ type: "gm_think", sid, text: "" }));
        update(i, { text: arr[i].text + d.text });
      } else if (d.channel === "gm_narration") {
        const i = ensureIdx(sid, "narration", () => ({ type: "narration", sid, text: "" }));
        update(i, { text: arr[i].text + d.text });
      } else if (d.channel === "npc_speech") {
        const i = idxOf(sid, "npc");
        if (i != null) update(i, { revealed: true, speech: arr[i].speech + d.text });
      }
    } else if (k === "gm_tool_call") {
      push({ type: "tool", name: d.name, args: d.arguments });
    } else if (k === "gm_thinking") {
      if (d && d.trim()) {
        const i = ensureIdx(sid, "gm_think", () => ({ type: "gm_think", sid, text: "" }));
        update(i, { text: d });
      }
    } else if (k === "world_fact") {
      push({ type: "fact", text: d });
    } else if (k === "scene_update") {
      if (d?.title || d?.scene_id) {
        push({
          type: "scene_update",
          scene_id: d.scene_id,
          title: d.title,
          description: d.description,
          present_npcs: d.present_npcs || [],
        });
      } else {
        push({
          type: "scene_update",
          name: d.name,
          present: d.present,
          present_npcs: d.present_npcs || [],
        });
      }
    } else if (k === "npc_whereabouts") {
      push({
        type: "npc_whereabouts",
        name: d.name,
        present: d.present,
        current_scene: d.current_scene,
        whereabouts: d.whereabouts || {},
      });
    } else if (k === "dice") {
      push({ type: "dice", text: d });
    } else if (k === "tool_result") {
      /* internal, not rendered */
    } else if (k === "npc_start") {
      ensureIdx(sid, "npc", () => ({
        type: "npc",
        sid,
        name: a,
        speech: "",
        typing: true,
        revealed: false,
        action: null,
        claims: null,
        hidden: null,
      }));
    } else if (k === "npc_thinking") {
      const i = idxOf(sid, "npc");
      if (i != null) update(i, { hidden: d });
    } else if (k === "npc_speech") {
      const i = idxOf(sid, "npc");
      if (i != null) {
        update(i, {
          typing: false,
          revealed: true,
          speech: d.speech,
          action: d.action || null,
          claims: d.claims || [],
        });
      }
    } else if (k === "gm_reject") {
      push({ type: "reject", name: a, reason: d });
    } else if (k === "gm_narration") {
      if (d && d.trim()) {
        const i = ensureIdx(sid, "narration", () => ({ type: "narration", sid, text: "" }));
        update(i, { text: d });
      }
    } else if (k === "meta") {
      push({ type: "meta", data: d });
    } else if (k === "meta_total") {
      push({ type: "meta_total", data: d });
    } else if (k === "error") {
      push({ type: "error", agent: a, text: d });
    }
  }

  return {
    subscribe(fn) {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
    getSnapshot() {
      return pub;
    },
    // single live event -> coalesced render
    dispatch(ev) {
      applyEvent(ev);
      schedule();
    },
    // batch (restore transcript) -> one render
    dispatchMany(events) {
      events.forEach(applyEvent);
      notify();
    },
    // start of a new turn: keep messages, drop streaming targets
    beginTurn() {
      liveIdx = new Map();
    },
    // full wipe (reset / new / before restore)
    clear() {
      arr = [];
      liveIdx = new Map();
      nextId = 1;
      notify();
    },
    // append a synthetic local message (e.g. "Новая партия")
    pushLocal(msg) {
      push(msg);
      notify();
    },
  };
}
