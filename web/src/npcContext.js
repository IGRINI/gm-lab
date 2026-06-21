import { createContext } from "react";

// Roster of NPCs for the current world: [{ id, name, ... }].
// Tool cards consume this to turn raw `npc_id`s (e.g. "borin") from tool-call
// arguments into player-facing names ("Борин") with their assigned color.
export const NpcRosterContext = createContext([]);
