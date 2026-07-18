import assert from "node:assert/strict";
import test from "node:test";

import { isMessageVisible, toolMode } from "../src/devSettings.js";

const playerVisibility = { toolCalls: false, memoryOps: false };

test("generic player update keeps its compact player card", () => {
  assert.equal(
    toolMode("update_character", playerVisibility, { result: { target: "player" } }),
    "player"
  );
});

test("generic NPC update stays hidden outside developer mode", () => {
  assert.equal(
    toolMode("update_character", playerVisibility, { result: { target: "npc" } }),
    "hidden"
  );
});

test("legacy player update remains player-visible", () => {
  assert.equal(toolMode("update_player_character", playerVisibility), "player");
});

test("developer tool calls show NPC updates", () => {
  assert.equal(
    toolMode("update_character", { toolCalls: true, memoryOps: true }, { args: { target: "npc" } }),
    "full"
  );
});

test("curated player actions are visible while their result is pending", () => {
  const message = { type: "tool", name: "travel_to", args: {}, result: undefined };
  assert.equal(toolMode(message.name, playerVisibility, message), "player");
  assert.equal(isMessageVisible(message, playerVisibility), true);
});

test("generator and NPC-internal tools remain hidden from players", () => {
  for (const name of ["generate_location", "generate_npc", "move_npc", "ask_npc"]) {
    const message = { type: "tool", name, args: {}, result: undefined };
    assert.equal(toolMode(name, playerVisibility, message), "hidden");
    assert.equal(isMessageVisible(message, playerVisibility), false);
  }
});
