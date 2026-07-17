import assert from "node:assert/strict";
import test from "node:test";
import { normalizeEntities, resolveEntity } from "../src/entityContext.js";

test("NPC entities inherit the live roster portrait without losing entity metadata", () => {
  const registry = normalizeEntities(
    {
      entities: [{ kind: "npc", id: "borin", title: "Борин", meta: [{ label: "роль", value: "трактирщик" }] }],
    },
    [{ id: "borin", name: "Борин", portrait_url: "/portraits/borin.webp", condition: "насторожен" }]
  );

  assert.deepEqual(resolveEntity(registry, "npc", "borin"), {
    id: "borin",
    name: "Борин",
    portrait_url: "/portraits/borin.webp",
    condition: "насторожен",
    kind: "npc",
    title: "Борин",
    meta: [{ label: "роль", value: "трактирщик" }],
    key: "npc:borin",
  });
});
