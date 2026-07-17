import assert from "node:assert/strict";
import test from "node:test";

import { toolMode } from "../src/devSettings.js";

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
