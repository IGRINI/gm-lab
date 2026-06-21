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
  let live = false; // true while folding a live streamed event (vs restoring history)
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

  // A tool's result arrives as a separate event right after its gm_tool_call.
  // Attach it to that pending tool row so call + result render as ONE card.
  // Scans from the end for the nearest matching tool call still awaiting a result.
  const attachResult = (toolName, payload, extra) => {
    for (let i = arr.length - 1; i >= 0; i--) {
      const m = arr[i];
      if (m.type === "tool" && m.name === toolName && m.result === undefined) {
        arr[i] = { ...m, result: payload, ...(extra || null) };
        return true;
      }
    }
    return false;
  };
  // Attach to the matching call, or fall back to a standalone result card.
  const toolResult = (toolName, payload) => {
    if (!attachResult(toolName, payload)) push({ type: "tool_result", name: toolName, payload });
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
      if (d?.name === "ask_player") return;
      // result stays undefined until the tool's outcome event arrives and attaches.
      push({ type: "tool", name: d.name, args: d.arguments, result: undefined });
    } else if (k === "gm_thinking") {
      if (d && d.trim()) {
        const i = ensureIdx(sid, "gm_think", () => ({ type: "gm_think", sid, text: "" }));
        update(i, { text: d });
      }
    } else if (k === "world_fact") {
      // New backend streams a structured payload; old transcripts streamed a string.
      if (d && typeof d === "object") toolResult("get_world_fact", d);
      else push({ type: "fact", text: d });
    } else if (k === "world_state_update") {
      toolResult("update_world_state", d);
    } else if (k === "world_query") {
      toolResult("query_world_state", d);
    } else if (k === "npc_profile") {
      toolResult("get_npc_profile", d);
    } else if (k === "time") {
      toolResult("advance_time", d);
    } else if (k === "player_character_update") {
      toolResult("update_player_character", d);
    } else if (k === "tool_search") {
      toolResult("tool_search", { text: d });
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
      // New backend streams the full roll payload (faces, kept dice, grade) so the UI
      // can animate real dice; old transcripts streamed just the detail string.
      if (d && typeof d === "object") {
        // resultLive=true only for a live roll, so restored history snaps instead of re-tumbling.
        if (!attachResult("roll_dice", d, { resultLive: live })) {
          push({ type: "dice_roll", roll: d, resultLive: live });
        }
      } else push({ type: "dice", text: d });
    } else if (k === "tool_result") {
      /* internal, not rendered */
    } else if (k === "npc_start") {
      ensureIdx(sid, "npc", () => ({
        type: "npc",
        sid,
        name: a,
        npc_id: d?.npc_id,
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
    // single live event -> coalesced render (dice from a live roll should animate)
    dispatch(ev) {
      live = true;
      applyEvent(ev);
      live = false;
      schedule();
    },
    // batch (restore transcript) -> one render (historical dice snap, never re-tumble)
    dispatchMany(events) {
      live = false;
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
