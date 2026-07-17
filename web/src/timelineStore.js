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
  let turnCheckpoint = null;
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
  const toolResult = (toolName, payload, aliases = []) => {
    if (attachResult(toolName, payload)) return;
    for (const alias of aliases) {
      if (attachResult(alias, payload)) return;
    }
    push({ type: "tool_result", name: toolName, payload });
  };

  function applyEvent(ev) {
    const k = ev.kind;
    const a = ev.agent;
    const d = ev.data;
    const sid = ev.sid;

    if (k === "player") {
      push({
        type: "player",
        text: d,
        turn: Number.isInteger(ev.turn) ? ev.turn : null,
        rewindable: ev.rewindable === true,
      });
    } else if (k === "delta") {
      if (d.channel === "gm_thinking") {
        const i = ensureIdx(sid, "gm_think", () => ({ type: "gm_think", sid, text: "" }));
        update(i, { text: arr[i].text + d.text });
      } else if (d.channel === "gm_narration") {
        const i = ensureIdx(sid, "narration", () => ({ type: "narration", sid, text: "" }));
        update(i, { text: arr[i].text + d.text });
      } else if (d.channel === "npc_speech") {
        const i = idxOf(sid, "npc");
        if (i != null)
          update(i, {
            revealed: true,
            response: (arr[i].response || "") + d.text,
            speech: arr[i].speech + d.text,
          });
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
    } else if (k === "character_update") {
      // The generic event/tool pair supersedes the player-only one. The alias
      // keeps a restored legacy call attachable if its result was saved under
      // the new event kind.
      toolResult("update_character", d, ["update_player_character"]);
    } else if (k === "player_character_update") {
      // Old transcripts retain their original card name, while a mixed-version
      // history can still attach the result to the generic call.
      toolResult("update_player_character", d, ["update_character"]);
    } else if (k === "tool_search") {
      toolResult("tool_search", { text: d });
    } else if (k === "scene_update") {
      if (d?.title || d?.scene_id) {
        push({
          type: "scene_update",
          scene: { ...d, present_npcs: d.present_npcs || [] },
          scene_id: d.scene_id,
          location_id: d.location_id,
          title: d.title,
          description: d.description,
          image_url: d.image_url,
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
        response: "",
        beats: [],
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
        const response =
          typeof d.response === "string" && d.response.trim()
            ? d.response
            : [d.action, d.speech].filter((item) => item && String(item).trim()).join(" ");
        update(i, {
          typing: false,
          revealed: true,
          response,
          beats: Array.isArray(d.beats) ? d.beats : [],
          speech: d.speech || "",
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
      // A canonical restore supersedes any optimistic streamed attempt.
      turnCheckpoint = null;
      live = false;
      events.forEach(applyEvent);
      notify();
    },
    // Start an optimistic turn. Its streamed rows remain visible on a retryable
    // failure, then rollbackTurn removes the whole attempt before retrying.
    beginTurn() {
      if (turnCheckpoint) {
        throw new Error("timeline turn already active");
      }
      turnCheckpoint = {
        arr: arr.slice(),
        liveIdx: new Map(liveIdx),
        nextId,
      };
      liveIdx = new Map();
    },
    commitTurn() {
      if (!turnCheckpoint) return false;
      turnCheckpoint = null;
      return true;
    },
    // A successful live turn can be marked from the terminal receipt without
    // reloading the whole transcript. Canonical restores still carry these
    // fields directly on the replayed player event.
    markLatestPlayerRewindable(turn) {
      if (!Number.isInteger(turn) || turn <= 0) return false;
      for (let index = arr.length - 1; index >= 0; index--) {
        if (arr[index].type !== "player") continue;
        update(index, { turn, rewindable: true });
        notify();
        return true;
      }
      return false;
    },
    rollbackTurn() {
      if (!turnCheckpoint) return false;
      arr = turnCheckpoint.arr;
      liveIdx = turnCheckpoint.liveIdx;
      nextId = turnCheckpoint.nextId;
      turnCheckpoint = null;
      notify();
      return true;
    },
    // Temporarily show the prefix before an edited/branched player turn. The
    // server keeps the canonical chat untouched until the replacement turn is
    // committed; callers reload it if that staged operation fails or stops.
    truncateFromPlayerTurn(turn) {
      if (turnCheckpoint || !Number.isInteger(turn) || turn <= 0) return false;
      const index = arr.findIndex(
        (message) => message.type === "player" && message.turn === turn
      );
      if (index < 0) return false;
      arr = arr.slice(0, index);
      liveIdx = new Map();
      notify();
      return true;
    },
    // Publish any rAF-coalesced rows before UI state (for example a retry
    // action) starts referring to the latest streamed message.
    flush() {
      notify();
    },
    // full wipe (reset / new / before restore)
    clear() {
      arr = [];
      liveIdx = new Map();
      nextId = 1;
      turnCheckpoint = null;
      notify();
    },
    // append a synthetic local message (e.g. "Новая партия")
    pushLocal(msg) {
      push(msg);
      notify();
    },
  };
}
