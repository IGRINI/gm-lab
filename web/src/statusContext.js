import { createContext } from "react";

// Player-facing NPC-whereabouts status labels (status key -> RU label), delivered
// from the backend via /state (world.WHEREABOUTS_STATUS_LABELS). Single source of
// truth: components read labels from here instead of hardcoding a status table.
export const StatusLabelsContext = createContext({});
