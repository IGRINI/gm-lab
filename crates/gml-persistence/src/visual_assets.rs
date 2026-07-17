//! Chat-scoped generated visuals.
//!
//! These references belong to a dialog rather than to reusable world/story
//! packages: generated files are stored by the application and the chat keeps
//! only their stable serving URLs plus provider metadata.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One generated image persisted by the application.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogVisualAsset {
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
}

/// Generated portraits and location art owned by one saved dialog.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogVisualAssets {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub characters: BTreeMap<String, DialogVisualAsset>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub locations: BTreeMap<String, DialogVisualAsset>,
}

impl DialogVisualAssets {
    pub fn is_empty(&self) -> bool {
        self.characters.is_empty() && self.locations.is_empty()
    }
}
