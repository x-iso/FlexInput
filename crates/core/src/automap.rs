/// Semantically equivalent pin-ID groups across controller families.
/// Each inner slice lists IDs that represent the same physical control
/// (e.g. "btn_south" / "btn_cross" / "btn_b" are all "bottom face button").
const SEMANTIC_GROUPS: &[&[&str]] = &[
    // Face buttons
    &["btn_south",  "btn_cross",    "btn_b"],   // A  / Cross / B(Nintendo)
    &["btn_east",   "btn_circle",   "btn_a"],   // B  / Circle / A(Nintendo)
    &["btn_west",   "btn_square",   "btn_y"],   // X  / Square / Y(Nintendo)
    &["btn_north",  "btn_triangle", "btn_x"],   // Y  / Triangle / X(Nintendo)
    // Shoulder bumpers
    &["btn_lb", "btn_l1", "btn_l"],
    &["btn_rb", "btn_r1", "btn_r"],
    // Triggers (XInput Float, DS4 Float "l2"/"r2", Switch Pro digital Bool "btn_zl"/"btn_zr")
    &["left_trigger",  "l2", "btn_zl"],
    &["right_trigger", "r2", "btn_zr"],
    // Stick clicks
    &["btn_ls", "btn_l3"],
    &["btn_rs", "btn_r3"],
    // Menu / system
    &["btn_start", "btn_options", "btn_plus"],
    &["btn_back",  "btn_share",   "btn_minus"],
    &["btn_guide", "btn_ps",      "btn_home"],
];

/// Given lists of source and destination pin IDs, returns `(src_id, dst_id)` pairs
/// for every auto-mappable signal.  Direct ID match has priority over semantic-group
/// match so that same-family devices round-trip without any translation.
pub fn resolve_mapping<'a>(src_pins: &[&'a str], dst_pins: &[&'a str]) -> Vec<(&'a str, &'a str)> {
    let mut result = Vec::new();
    let mut claimed_dst = std::collections::HashSet::new();

    for &src_id in src_pins {
        // 1. Direct ID match (same controller family or shared names like "dpad_up")
        if let Some(&dst_id) = dst_pins.iter().find(|&&d| d == src_id) {
            if claimed_dst.insert(dst_id) {
                result.push((src_id, dst_id));
            }
            continue;
        }

        // 2. Semantic group match (cross-family, e.g. "btn_b" → "btn_south")
        if let Some(group) = SEMANTIC_GROUPS.iter().find(|g| g.contains(&src_id)).copied() {
            if let Some(&dst_id) = dst_pins.iter().find(|&&d| group.contains(&d)) {
                if claimed_dst.insert(dst_id) {
                    result.push((src_id, dst_id));
                }
            }
        }
    }

    result
}
