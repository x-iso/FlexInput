use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::signal::SignalType;

pub const PATCH_VERSION: u32 = 1;

/// The top-level document saved as a .fxp file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Patch {
    pub version: u32,
    pub nodes: Vec<NodeInstance>,
    pub wires: Vec<Wire>,
}

impl Default for Patch {
    fn default() -> Self {
        Self { version: PATCH_VERSION, nodes: vec![], wires: vec![] }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInstance {
    pub id: Uuid,
    /// Matches ModuleDescriptor::id. Use "subpatch" for inline sub-patches.
    pub module_id: String,
    pub position: [f32; 2],
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub params: HashMap<String, serde_json::Value>,
    /// Inline sub-patch definition, present only when module_id == "subpatch".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subpatch: Option<Box<SubPatch>>,
}

/// A reusable sub-patch with declared I/O — saved standalone as a .fxm file.
/// Can be embedded inline inside a NodeInstance or referenced by path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubPatch {
    pub display_name: String,
    pub pins_in: Vec<SubPatchPin>,
    pub pins_out: Vec<SubPatchPin>,
    pub patch: Patch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubPatchPin {
    pub name: String,
    pub signal_type: SignalType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wire {
    pub from_node: Uuid,
    pub from_pin: String,
    pub to_node: Uuid,
    pub to_pin: String,
}
