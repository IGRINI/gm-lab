<< snapshot_header >>

TIME STATE:
<< time_state >>

DYNAMIC NPC ROSTER (relevant/nearby now; tool ids; internal_name is GM-only unless player_label matches it; use read_state(roster) for the full list):
<< roster >><% if public_facts %>

CURRENT PUBLIC FACTS:
<< public_facts >><% endif %>

PLAYER CHARACTER CARD (current sheet; GM-only notes may be present):
<< player_card >>

CURRENT SCENE STATE:
<< scene_context >><% if canon_world %>

CANON WORLD (structured truth — region, settlement, factions, recent history):
<< canon_world >><% endif %><% if memory_context %>

LIVING MEMORY SNAPSHOT:
<< memory_context >><% endif %>

ENTITY REFERENCE MARKUP:
<< entity_refs >><% if constraints %>

SCENE CONSTRAINTS (must enforce when reviewing NPC responses):
<< constraints >><% endif %>

PLAYER OPTION SUGGESTIONS:
<< options_state >>
