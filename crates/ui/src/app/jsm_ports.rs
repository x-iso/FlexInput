//! The macros a JSM Config owns: one per `@Name` its config uses that nothing
//! else in the tab already answers to.
//!
//! A config binds `S = @Jump` and reads `@Jump,E = SPACE`; both need a macro
//! called Jump. Rather than make you add one somewhere, the JSM node owns it —
//! stored on the node in the Macro Output port schema (`jsm_macros`), listed in
//! the tab's macro registry, so it is a target for the Remapper, Touch Zones,
//! every picker and every other JSM config in the tab exactly as a Macro
//! Output's port is. Its header's Macros button gives each one an icon, and can
//! show it as an output pin.
//!
//! When this runs matters more than how:
//!
//! * Only at a point where a name is finished: the caret leaving it, an editor
//!   letting go of the keyboard, a word arriving from the pad's keyboard — and
//!   a config seen for the first time, which is a patch being loaded. `@T`,
//!   `@TE`, `@TES` must not each become a macro on the way to `@TEST`.
//! * While someone types, only the config being typed in is touched, and the
//!   name under the caret is left alone.
//!
//! Where it runs matters too. An open sub-patch window edits its own copy of
//! the sub-patch and hands it back to the tab only once the typing stops, so
//! mid-typing the tab's copy of that config is the text from before. Each open
//! window is therefore a view of its own, the authority on its sub-patch; a
//! change is written to every copy of the node it changes, so whichever copy
//! wins the next hand-over has it.
//!
//! A name changed where it stands renames its macro — the same id, so its icon,
//! its pin and everything pointed at it come along — provided nothing in the
//! tab still uses the old name. And it tidies up after itself: a macro a node
//! made that no config names any more goes, unless you have adopted it — given
//! it an icon, shown it as a pin, or pointed something else at it.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use flexinput_core::macros::MacroPortDef;

use super::*;
use crate::canvas::viewer::JsmTyping;

impl FlexInputApp {
    /// Once a frame, before the macro registry is published (so a config
    /// resolves against its new macros on the next compile).
    pub(crate) fn sync_jsm_macro_ports(&mut self, ctx: &egui::Context) {
        // The engine pauses a config's keys while its editor is being typed in;
        // both that and this follow one answer to "is anyone typing".
        let typing = crate::canvas::viewer::jsm_editor_focused_recently(ctx);
        flexinput_engine::eval::set_jsm_editor_focus(typing);
        let requested = crate::canvas::viewer::take_jsm_port_sync_request(ctx);
        // While typing, only a finished name is acted on: the editor asks when
        // the caret leaves one, and the one it is still in is left alone.
        let mode = if typing {
            if !requested {
                return;
            }
            let Some(t) = crate::canvas::viewer::jsm_typing(ctx) else { return };
            Mode::Typing(t)
        } else {
            Mode::Settled
        };
        let seen_id = egui::Id::new("jsm_port_sync_seen");
        let mut seen: HashMap<u64, Synced> = ctx.data(|d| d.get_temp(seen_id)).unwrap_or_default();
        let mut known: Vec<String> =
            self.macro_display_entries().into_iter().map(|e| e.name).collect();
        let active = self.active_tab;
        let paths: Vec<Option<Path>> =
            (0..self.sub_patch_editors.len()).map(|i| editor_path(&self.sub_patch_editors, i, active)).collect();
        let mut views = vec![View { prefix: Vec::new(), snarl: &mut self.tabs[active].canvas.snarl }];
        let mut editor_of = vec![None];
        for (i, ed) in self.sub_patch_editors.iter_mut().enumerate() {
            if let Some(prefix) = paths[i].clone() {
                views.push(View { prefix, snarl: &mut ed.canvas.snarl });
                editor_of.push(Some(i));
            }
        }
        let bump = sync_views(&mut views, active as u64, requested, &mode, &mut known, &mut seen);
        drop(views);
        for (v, b) in bump.iter().enumerate() {
            if !*b {
                continue;
            }
            // A direct write: bump the generation, so the tab's copy reaches an
            // open sub-patch window — or a window's reaches the tab.
            let canvas = match editor_of[v] {
                None => &mut self.tabs[active].canvas,
                Some(i) => &mut self.sub_patch_editors[i].canvas,
            };
            canvas.mutation_gen = canvas.mutation_gen.wrapping_add(1);
        }
        if bump.iter().any(|b| *b) {
            ctx.request_repaint();
        }
        ctx.data_mut(|d| d.insert_temp(seen_id, seen));
    }
}

/// Where an open sub-patch window's sub-patch sits in the tab: the sub-patch
/// nodes on the way down. None for a window of another tab.
fn editor_path(eds: &[SubPatchEditor], i: usize, tab: usize) -> Option<Path> {
    let mut path = Vec::new();
    let mut cur = Some(i);
    while let Some(j) = cur {
        let e = eds.get(j)?;
        if e.tab_idx != tab || path.len() > 64 {
            return None;
        }
        path.push(e.node_id);
        cur = e.parent_editor_idx;
    }
    path.reverse();
    Some(path)
}

/// How a sync runs.
pub(crate) enum Mode {
    /// No editor has the keyboard: every due config is brought fully up to
    /// date, its unused macros tidied away included.
    Settled,
    /// Someone is typing, and a name they were typing is finished: the config
    /// being typed in gets macros for its finished names — never the one the
    /// caret is still in — or renames the ones they replaced. Nothing is
    /// removed, since a name being retyped (`@Jum` on the way back to `@Jump`)
    /// isn't gone; the full sync waits for the typing to stop.
    Typing(JsmTyping),
}

/// What a node looked like when last synced.
#[derive(Clone, Default)]
pub(crate) struct Synced {
    /// Its texts, as of the last full sync; None until one.
    hash: Option<u64>,
    /// The `@Name`s it used then — what a new name may have been renamed from.
    names: Vec<String>,
}

/// One copy of the tab's patch: the tab itself, or an open sub-patch window's
/// copy of the sub-patch at `prefix`.
pub(crate) struct View<'a> {
    pub(crate) prefix: Path,
    pub(crate) snarl: &'a mut Snarl<NodeData>,
}

/// Every config text of one JSM node, all tabs.
fn jsm_texts(node: &NodeData) -> Vec<&str> {
    node.params
        .get("jsm_tabs")
        .and_then(|v| v.as_array())
        .map(|tabs| tabs.iter().filter_map(|t| t.get("text").and_then(|s| s.as_str())).collect())
        .unwrap_or_default()
}

/// A JSM node's own macros.
fn owned(node: &NodeData) -> Vec<MacroPortDef> {
    flexinput_core::macros::ports_from_value(node.params.get(flexinput_engine::eval::JSM_MACROS_PARAM))
}

fn shown_pins(node: &NodeData) -> Vec<String> {
    node.params
        .get(flexinput_engine::eval::JSM_MACRO_OUTS_PARAM)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|p| p.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// Where a node sits: the sub-patch nodes on the way down, then its own id.
pub(crate) type Path = Vec<egui_snarl::NodeId>;

/// One JSM node as the first pass saw it, in the view that has the say on it.
struct Seen {
    view: usize,
    path: Path,
    key: u64,
    hash: u64,
    /// Each of its texts on its own, to know the one being typed in.
    text_hashes: Vec<u64>,
    names: Vec<String>,
}

/// A macro some JSM node owns.
struct Owner {
    view: usize,
    path: Path,
    id: String,
    name: String,
}

/// What the first pass gathers from the whole tab.
#[derive(Default)]
struct Survey {
    jsm: Vec<Seen>,
    /// Every `@Name` any config in the tab uses, lower-cased.
    mentioned: HashSet<String>,
    owners: Vec<Owner>,
    /// Every node's params as JSON, by path — for "does anything else point at
    /// this macro".
    params_json: Vec<(Path, String)>,
}

#[allow(clippy::too_many_arguments)]
fn survey(
    snarl: &Snarl<NodeData>,
    tab_key: u64,
    view: usize,
    prefix: &Path,
    windows: &HashSet<Path>,
    json: bool,
    out: &mut Survey,
) {
    for (id, node) in snarl.node_ids() {
        let mut path = prefix.clone();
        path.push(id);
        // Only the tidy-up reads these, and only once nobody is typing.
        if json {
            out.params_json.push((path.clone(), serde_json::to_string(&node.params).unwrap_or_default()));
        }
        if node.module_id == "module.jsm" {
            let texts = jsm_texts(node);
            let mut h = std::collections::hash_map::DefaultHasher::new();
            texts.hash(&mut h);
            let mut k = std::collections::hash_map::DefaultHasher::new();
            (tab_key, &path).hash(&mut k);
            let mut names: Vec<String> = Vec::new();
            for text in &texts {
                for name in flexinput_engine::eval::jsm_at_names(text) {
                    out.mentioned.insert(name.trim().to_lowercase());
                    if !names.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                        names.push(name);
                    }
                }
            }
            for p in owned(node) {
                out.owners.push(Owner { view, path: path.clone(), id: p.id, name: p.name });
            }
            out.jsm.push(Seen {
                view,
                path: path.clone(),
                key: k.finish(),
                hash: h.finish(),
                text_hashes: texts.iter().map(|t| crate::canvas::viewer::jsm_text_hash(t)).collect(),
                names,
            });
        }
        // A sub-patch open in a window is that window's to report.
        if let Some(sp) = node.subpatch.as_ref() {
            if !windows.contains(&path) {
                survey(&sp.snarl, tab_key, view, &path, windows, json, out);
            }
        }
    }
}

fn node_at_mut<'a>(snarl: &'a mut Snarl<NodeData>, path: &[egui_snarl::NodeId]) -> Option<&'a mut NodeData> {
    let (last, down) = path.split_last()?;
    let mut s = snarl;
    for id in down {
        s = &mut s.get_node_mut(*id)?.subpatch.as_mut()?.snarl;
    }
    s.get_node_mut(*last)
}

/// Change the macros of the JSM node at `path`, as `view` has them, and write
/// the result to every copy of the node. `edit` says whether it changed
/// anything.
fn edit_ports(
    views: &mut [View<'_>],
    view: usize,
    path: &[egui_snarl::NodeId],
    touched: &mut [bool],
    edit: impl FnOnce(&mut Vec<MacroPortDef>) -> bool,
) {
    let Some(local) = path.strip_prefix(views[view].prefix.as_slice()) else { return };
    let Some(node) = node_at_mut(views[view].snarl, local) else { return };
    let mut ports = owned(node);
    if !edit(&mut ports) {
        return;
    }
    let value = flexinput_core::macros::ports_to_value(&ports);
    for (v, copy) in views.iter_mut().enumerate() {
        let Some(local) = path.strip_prefix(copy.prefix.as_slice()) else { continue };
        if let Some(node) = node_at_mut(copy.snarl, local) {
            node.params.insert(flexinput_engine::eval::JSM_MACROS_PARAM.to_string(), value.clone());
            touched[v] = true;
        }
    }
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Bring every JSM node in the views that is due — seen for the first time, or
/// changed since its last sync when a sync was `requested`; while typing, the
/// one being typed in — up to date: a macro for each `@Name` nothing answers
/// to (or the macro of a name it replaced, renamed), and none for names no
/// config uses any more, unless adopted. `known` is every macro name the tab
/// resolves (Macro Output ports, Virtual Menu entries, JSM macros); `seen`
/// remembers each node as last synced. Returns, per view, whether its
/// generation should be bumped.
pub(crate) fn sync_views(
    views: &mut [View<'_>],
    tab_key: u64,
    requested: bool,
    mode: &Mode,
    known: &mut Vec<String>,
    seen: &mut HashMap<u64, Synced>,
) -> Vec<bool> {
    use flexinput_core::macros as mac;
    let (settled, typing) = match mode {
        Mode::Settled => (true, None),
        Mode::Typing(t) => (false, Some(t)),
    };
    let skip = typing.and_then(|t| t.name.as_deref());
    let windows: HashSet<Path> = views.iter().skip(1).map(|v| v.prefix.clone()).collect();
    let mut s = Survey::default();
    for (v, view) in views.iter().enumerate() {
        survey(view.snarl, tab_key, v, &view.prefix, &windows, settled, &mut s);
    }
    // A window's macros count before its copy reaches the tab.
    for o in &s.owners {
        if !known.iter().any(|k| same(k, &o.name)) {
            known.push(o.name.clone());
        }
    }

    let mut touched = vec![false; views.len()];
    let mut typed_view = None;
    // Who is due, and what each used when last synced.
    let mut due: Vec<(usize, Synced)> = Vec::new();
    for (j, node_seen) in s.jsm.iter().enumerate() {
        let last = seen.get(&node_seen.key).cloned().unwrap_or_default();
        let is_due = match typing {
            None => match last.hash {
                None => true,
                Some(h) => requested && h != node_seen.hash,
            },
            Some(t) => {
                requested
                    && node_seen.path.last() == Some(&t.node)
                    && node_seen.text_hashes.contains(&t.text)
            }
        };
        if is_due {
            if typing.is_some() {
                typed_view = Some(node_seen.view);
            }
            due.push((j, last));
        }
    }

    // First every due config's renames and new macros, then the tidy-up — so a
    // macro one config made and another renamed isn't dropped by the first as
    // unused before the second gets to it.
    let mut kept_gone: Vec<Vec<String>> = Vec::new();
    for (j, last) in &due {
        let node_seen = &s.jsm[*j];
        // The names it uses that nothing answers to — the one under the caret
        // included, as a place a rename can land — and the JSM-made names it
        // used last time that nothing in the tab uses now.
        let fresh: Vec<&String> = node_seen.names.iter().filter(|n| !known.iter().any(|k| same(k, n))).collect();
        let mut gone: Vec<usize> = Vec::new();
        for old in &last.names {
            if s.mentioned.contains(&old.trim().to_lowercase()) {
                continue;
            }
            if let Some(i) = s.owners.iter().position(|o| same(&o.name, old)) {
                if !gone.contains(&i) {
                    gone.push(i);
                }
            }
        }

        // Replaced where it stood: a name that went, paired in order with one
        // that came, is the same macro renamed.
        let mut handled: Vec<&String> = Vec::new();
        let mut waiting: Vec<String> = Vec::new();
        for (i, new) in gone.iter().copied().zip(fresh.iter().copied()) {
            let (view, path, id, old) = {
                let o = &s.owners[i];
                (o.view, o.path.clone(), o.id.clone(), o.name.clone())
            };
            handled.push(new);
            // Still being typed — or, mid-typing, owned in another window, which
            // can't be written to without undoing that window's own edits:
            // later.
            if skip.is_some_and(|k| same(k, new)) || (!settled && view != node_seen.view) {
                waiting.push(old);
                continue;
            }
            edit_ports(views, view, &path, &mut touched, |ports| {
                match ports.iter_mut().find(|p| p.id == id) {
                    Some(p) => {
                        p.name = new.clone();
                        true
                    }
                    None => false,
                }
            });
            known.retain(|k| !same(k, &old));
            known.push(new.clone());
            s.owners[i].name = new.clone();
        }
        kept_gone.push(waiting);

        // Named, but nothing answers to it: make it here.
        let mut new_ports: Vec<MacroPortDef> = Vec::new();
        for name in fresh.iter().copied() {
            if handled.iter().any(|m| same(m, name)) || skip.is_some_and(|k| same(k, name)) {
                continue;
            }
            new_ports.push(MacroPortDef {
                id: mac::new_port_id(),
                name: name.clone(),
                icon: String::new(),
                icon_svg: String::new(),
                // Any, because a JSM macro carries whatever drives it — a press,
                // a trigger's pull — and whatever reads it decides: a JSM button
                // thresholds the amount, a wire takes the value as it is.
                signal_type: SignalType::Any,
            });
            known.push(name.clone());
        }
        if !new_ports.is_empty() {
            edit_ports(views, node_seen.view, &node_seen.path, &mut touched, |ports| {
                ports.extend(new_ports);
                true
            });
        }
    }

    // Gone from every config, and not adopted: drop it. Only once nobody is
    // typing.
    if settled {
        for (j, _) in &due {
            let node_seen = &s.jsm[*j];
            let me = &node_seen.path;
            let shown = node_at_mut(
                views[node_seen.view].snarl,
                me.strip_prefix(views[node_seen.view].prefix.as_slice()).unwrap_or(&[]),
            )
            .map(|n| shown_pins(n))
            .unwrap_or_default();
            let mut dropped: Vec<String> = Vec::new();
            edit_ports(views, node_seen.view, me, &mut touched, |ports| {
                ports.retain(|p| {
                    let pin = mac::macro_pin_id(&p.id);
                    let named = s.mentioned.contains(&p.name.trim().to_lowercase());
                    let adopted = !p.icon.is_empty()
                        || !p.icon_svg.is_empty()
                        || shown.contains(&pin)
                        || s.params_json.iter().any(|(path, json)| path != me && json.contains(&pin));
                    if !(named || adopted) {
                        dropped.push(p.name.clone());
                    }
                    named || adopted
                });
                !dropped.is_empty()
            });
            known.retain(|k| !dropped.iter().any(|d| same(d, k)));
        }
    }

    // What each used, for the next sync to compare with — and mid-typing, the
    // names still waiting to be renamed, since the full sync hasn't come.
    for ((j, last), waiting) in due.into_iter().zip(kept_gone) {
        let node_seen = &s.jsm[j];
        let mut names = node_seen.names.clone();
        let hash = if settled {
            Some(node_seen.hash)
        } else {
            names.extend(waiting);
            last.hash
        };
        seen.insert(node_seen.key, Synced { hash, names });
    }

    match typed_view {
        // Mid-typing, only the copy being typed in is handed on: bumping another
        // would have its copy pulled over the window's, typing and all.
        Some(v) => (0..touched.len()).map(|i| i == v && touched.iter().any(|t| *t)).collect(),
        None => touched,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jsm_node(text: &str) -> NodeData {
        let mut n = NodeData::from(&flexinput_core::ModuleDescriptor {
            id: "module.jsm",
            display_name: "JSM Config",
            category: "Mapping",
            inputs: vec![],
            outputs: vec![],
        });
        set_text(&mut n, text);
        n
    }

    fn set_text(n: &mut NodeData, text: &str) {
        n.params.insert("jsm_tabs".into(), serde_json::json!([{ "name": "main", "text": text }]));
    }

    fn names(n: &NodeData) -> Vec<String> {
        owned(n).into_iter().map(|p| p.name).collect()
    }

    /// The whole tab as one view; whether anything changed.
    fn sync_snarl(
        snarl: &mut Snarl<NodeData>,
        requested: bool,
        mode: &Mode,
        known: &mut Vec<String>,
        seen: &mut HashMap<u64, Synced>,
    ) -> bool {
        let mut views = [View { prefix: Vec::new(), snarl }];
        sync_views(&mut views, 0, requested, mode, known, seen)[0]
    }

    /// Typing in `node`, its text now `text`, the caret in `name`.
    fn typing(node: egui_snarl::NodeId, text: &str, name: Option<&str>) -> Mode {
        Mode::Typing(JsmTyping {
            node,
            text: crate::canvas::viewer::jsm_text_hash(text),
            name: name.map(str::to_string),
        })
    }

    /// The `@Name`s a config uses become the node's own Any macros — bound to or
    /// read, once each — and no Macro Output node is spawned for them.
    #[test]
    fn a_configs_at_names_become_its_own_macros() {
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let id = snarl.insert_node(egui::pos2(0.0, 0.0), jsm_node("S = @Jump\n@Jump,E = @Dash\nN = @jump"));
        let (mut known, mut seen) = (Vec::new(), HashMap::new());
        assert!(sync_snarl(&mut snarl, false, &Mode::Settled, &mut known, &mut seen), "first sight syncs");
        assert_eq!(names(&snarl[id]), ["Jump", "Dash"]);
        assert!(owned(&snarl[id]).iter().all(|p| p.signal_type == SignalType::Any));
        assert_eq!(snarl.nodes().count(), 1, "no Macro Output node");
    }

    /// Typed a letter at a time, nothing is made until the text is committed —
    /// and then only the finished name.
    #[test]
    fn half_typed_names_make_nothing_until_committed() {
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let id = snarl.insert_node(egui::pos2(0.0, 0.0), jsm_node(""));
        let (mut known, mut seen) = (Vec::new(), HashMap::new());
        sync_snarl(&mut snarl, false, &Mode::Settled, &mut known, &mut seen);
        for partial in ["S = @T", "S = @TE", "S = @TES", "S = @TEST"] {
            set_text(&mut snarl[id], partial);
            assert!(!sync_snarl(&mut snarl, false, &Mode::Settled, &mut known, &mut seen), "{partial}: not committed");
        }
        assert!(names(&snarl[id]).is_empty());
        assert!(sync_snarl(&mut snarl, true, &Mode::Settled, &mut known, &mut seen), "committed");
        assert_eq!(names(&snarl[id]), ["TEST"]);
    }

    /// While typing, a finished name — the caret has left it — gets its macro
    /// at once, the one the caret is still in doesn't, and nothing is removed,
    /// since a name being retyped isn't gone.
    #[test]
    fn typing_makes_finished_names_and_leaves_the_one_under_the_caret() {
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let id = snarl.insert_node(egui::pos2(0.0, 0.0), jsm_node("S = @Old"));
        let (mut known, mut seen) = (Vec::new(), HashMap::new());
        sync_snarl(&mut snarl, false, &Mode::Settled, &mut known, &mut seen);
        assert_eq!(names(&snarl[id]), ["Old"]);

        // `@Old` being retyped — the caret is in it, at `@Ol` — and `@Jump`
        // finished on the line below.
        let text = "S = @Ol\nE = @Jump";
        set_text(&mut snarl[id], text);
        let mode = typing(id, text, Some("Ol"));
        assert!(!sync_snarl(&mut snarl, false, &mode, &mut known, &mut seen), "only on request");
        assert!(sync_snarl(&mut snarl, true, &mode, &mut known, &mut seen));
        let got = names(&snarl[id]);
        assert!(got.contains(&"Jump".to_string()), "the finished one: {got:?}");
        assert!(!got.contains(&"Ol".to_string()), "not the one under the caret: {got:?}");
        assert!(got.contains(&"Old".to_string()), "nothing removed mid-typing: {got:?}");

        // Typing stops on `@Dash`, `@Old` given back: the full pass finishes
        // the job and tidies what no config names.
        set_text(&mut snarl[id], "S = @Old\nE = @Jump\nN = @Dash");
        assert!(sync_snarl(&mut snarl, true, &Mode::Settled, &mut known, &mut seen));
        assert_eq!(names(&snarl[id]), ["Old", "Jump", "Dash"]);
    }

    /// Mid-typing, only the config being typed in is touched — another one
    /// waiting on a name is left for the full sync.
    #[test]
    fn typing_touches_only_the_config_being_typed_in() {
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let a = snarl.insert_node(egui::pos2(0.0, 0.0), jsm_node(""));
        let b = snarl.insert_node(egui::pos2(0.0, 100.0), jsm_node(""));
        let (mut known, mut seen) = (Vec::new(), HashMap::new());
        sync_snarl(&mut snarl, false, &Mode::Settled, &mut known, &mut seen);
        set_text(&mut snarl[a], "S = @Jump");
        set_text(&mut snarl[b], "E = @Dash");
        assert!(sync_snarl(&mut snarl, true, &typing(a, "S = @Jump", None), &mut known, &mut seen));
        assert_eq!(names(&snarl[a]), ["Jump"]);
        assert!(names(&snarl[b]).is_empty());
    }

    /// A name changed where it stood renames its macro: the same id, so its
    /// icon and everything pointed at it come along.
    #[test]
    fn a_name_changed_in_place_renames_its_macro() {
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let id = snarl.insert_node(egui::pos2(0.0, 0.0), jsm_node("S = @Jump\nE = @Dash"));
        let (mut known, mut seen) = (Vec::new(), HashMap::new());
        sync_snarl(&mut snarl, false, &Mode::Settled, &mut known, &mut seen);
        let before = owned(&snarl[id]);
        let mut iconed = before.clone();
        iconed[0].icon = "star".into();
        snarl[id].params.insert(
            flexinput_engine::eval::JSM_MACROS_PARAM.into(),
            flexinput_core::macros::ports_to_value(&iconed),
        );

        // Mid-typing, with the caret still in it: nothing yet.
        let text = "S = @Lea\nE = @Dash";
        set_text(&mut snarl[id], text);
        sync_snarl(&mut snarl, true, &typing(id, text, Some("Lea")), &mut known, &mut seen);
        assert_eq!(names(&snarl[id]), ["Jump", "Dash"]);

        // Finished — the caret left it: the same macro, renamed.
        let text = "S = @Leap\nE = @Dash\n";
        set_text(&mut snarl[id], text);
        assert!(sync_snarl(&mut snarl, true, &typing(id, text, None), &mut known, &mut seen));
        let after = owned(&snarl[id]);
        assert_eq!(names(&snarl[id]), ["Leap", "Dash"]);
        assert_eq!(after[0].id, before[0].id, "same macro");
        assert_eq!(after[0].icon, "star", "its icon came along");
        assert!(known.iter().any(|k| k == "Leap") && !known.iter().any(|k| k == "Jump"));

        // And the full sync after leaves it be.
        assert!(!sync_snarl(&mut snarl, true, &Mode::Settled, &mut known, &mut seen));
        assert_eq!(names(&snarl[id]), ["Leap", "Dash"]);
    }

    /// A name another config still uses isn't renamed out from under it: the
    /// new name gets a macro of its own.
    #[test]
    fn a_name_still_used_elsewhere_is_not_renamed() {
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let a = snarl.insert_node(egui::pos2(0.0, 0.0), jsm_node("S = @Jump"));
        let b = snarl.insert_node(egui::pos2(0.0, 100.0), jsm_node("@Jump,E = SPACE"));
        let (mut known, mut seen) = (Vec::new(), HashMap::new());
        sync_snarl(&mut snarl, false, &Mode::Settled, &mut known, &mut seen);
        let jump = owned(&snarl[a]).into_iter().chain(owned(&snarl[b])).next().unwrap();

        set_text(&mut snarl[a], "S = @Leap");
        sync_snarl(&mut snarl, true, &Mode::Settled, &mut known, &mut seen);
        let all: Vec<MacroPortDef> = owned(&snarl[a]).into_iter().chain(owned(&snarl[b])).collect();
        assert!(all.iter().any(|p| p.id == jump.id && p.name == "Jump"), "Jump kept for b: {all:?}");
        assert!(all.iter().any(|p| p.name == "Leap"), "Leap made: {all:?}");
    }

    /// The last config using a macro another config made renames that one.
    #[test]
    fn renaming_reaches_a_macro_another_config_made() {
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let a = snarl.insert_node(egui::pos2(0.0, 0.0), jsm_node("S = @Jump"));
        let (mut known, mut seen) = (Vec::new(), HashMap::new());
        sync_snarl(&mut snarl, false, &Mode::Settled, &mut known, &mut seen);
        let b = snarl.insert_node(egui::pos2(0.0, 100.0), jsm_node("@Jump,E = SPACE"));
        sync_snarl(&mut snarl, false, &Mode::Settled, &mut known, &mut seen);
        let jump = owned(&snarl[a])[0].clone();
        assert!(names(&snarl[b]).is_empty(), "b uses a's");

        set_text(&mut snarl[a], "S = A");
        set_text(&mut snarl[b], "@Leap,E = SPACE");
        sync_snarl(&mut snarl, true, &Mode::Settled, &mut known, &mut seen);
        let all: Vec<MacroPortDef> = owned(&snarl[a]).into_iter().chain(owned(&snarl[b])).collect();
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!((all[0].id.as_str(), all[0].name.as_str()), (jump.id.as_str(), "Leap"));
    }

    /// A macro the config stops naming goes — unless it was adopted: given an
    /// icon, shown as a pin, or pointed at by something else.
    #[test]
    fn unnamed_macros_go_unless_adopted() {
        use flexinput_core::macros as mac;
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let id = snarl.insert_node(egui::pos2(0.0, 0.0), jsm_node("S = @Jump\nE = @Iconed\nN = @Shown\nW = @Used"));
        let (mut known, mut seen) = (Vec::new(), HashMap::new());
        sync_snarl(&mut snarl, false, &Mode::Settled, &mut known, &mut seen);
        // Adopt three of the four.
        let mut ports = owned(&snarl[id]);
        ports[1].icon = "star".into();
        let shown_pin = mac::macro_pin_id(&ports[2].id);
        let used_pin = mac::macro_pin_id(&ports[3].id);
        snarl[id].params.insert(flexinput_engine::eval::JSM_MACROS_PARAM.into(), mac::ports_to_value(&ports));
        snarl[id].params.insert(flexinput_engine::eval::JSM_MACRO_OUTS_PARAM.into(), serde_json::json!([shown_pin]));
        let mut remapper = jsm_node("");
        remapper.module_id = "module.remapper".into();
        remapper.params.insert("mappings".into(), serde_json::json!([{ "out": [used_pin] }]));
        snarl.insert_node(egui::pos2(0.0, 100.0), remapper);

        // All four dropped from the config: Jump goes, the adopted three stay.
        set_text(&mut snarl[id], "S = A");
        assert!(sync_snarl(&mut snarl, true, &Mode::Settled, &mut known, &mut seen));
        assert_eq!(names(&snarl[id]), ["Iconed", "Shown", "Used"]);
    }

    /// Two configs naming one macro share it: it lives on the first, and stays
    /// while either still names it.
    #[test]
    fn two_configs_share_one_macro() {
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let a = snarl.insert_node(egui::pos2(0.0, 0.0), jsm_node("S = @Jump"));
        let b = snarl.insert_node(egui::pos2(0.0, 100.0), jsm_node("@Jump,E = SPACE"));
        let (mut known, mut seen) = (Vec::new(), HashMap::new());
        sync_snarl(&mut snarl, false, &Mode::Settled, &mut known, &mut seen);
        let total = names(&snarl[a]).len() + names(&snarl[b]).len();
        assert_eq!(total, 1, "one Jump between them");
        // The owner stops naming it; the other still does, so it stays.
        let owner = if names(&snarl[a]).is_empty() { b } else { a };
        let other = if owner == a { b } else { a };
        set_text(&mut snarl[owner], "S = A");
        set_text(&mut snarl[other], "@Jump,E = SPACE\nN = B");
        sync_snarl(&mut snarl, true, &Mode::Settled, &mut known, &mut seen);
        assert_eq!(names(&snarl[owner]), ["Jump"]);
    }

    /// A name the tab already resolves — a Macro Output port, a Virtual Menu
    /// entry — is left to it.
    #[test]
    fn names_the_tab_already_has_are_left_alone() {
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let id = snarl.insert_node(egui::pos2(0.0, 0.0), jsm_node("S = @reload\nE = @\"Menu — Show\""));
        let mut known = vec!["Reload".to_string(), "Menu — Show".to_string()];
        assert!(!sync_snarl(&mut snarl, false, &Mode::Settled, &mut known, &mut HashMap::new()));
        assert!(names(&snarl[id]).is_empty());
    }

    fn sub_patch(inner: Snarl<NodeData>) -> NodeData {
        let mut sp = jsm_node("");
        sp.module_id = "subpatch".into();
        sp.params.clear();
        sp.subpatch = Some(Box::new(crate::canvas::node::UiSubPatch {
            snarl: Box::new(inner),
            ..Default::default()
        }));
        sp
    }

    /// A JSM node inside a sub-patch owns its macros there.
    #[test]
    fn a_sub_patch_config_owns_its_macros() {
        let mut inner: Snarl<NodeData> = Snarl::new();
        let jsm = inner.insert_node(egui::pos2(0.0, 0.0), jsm_node("S = @Fire"));
        let mut outer: Snarl<NodeData> = Snarl::new();
        let sp_id = outer.insert_node(egui::pos2(0.0, 0.0), sub_patch(inner));
        assert!(sync_snarl(&mut outer, false, &Mode::Settled, &mut Vec::new(), &mut HashMap::new()));
        let inner = &outer.get_node(sp_id).unwrap().subpatch.as_ref().unwrap().snarl;
        assert_eq!(names(&inner[jsm]), ["Fire"]);
    }

    /// Typed in a sub-patch window, whose copy is a step ahead of the tab's: the
    /// window's text is what counts, the macro lands on both copies, and only
    /// the window is handed on — bumping the tab would pull its old text back
    /// over the typing.
    #[test]
    fn typing_in_a_sub_patch_window_works_on_the_windows_copy() {
        let mut inner: Snarl<NodeData> = Snarl::new();
        let jsm = inner.insert_node(egui::pos2(0.0, 0.0), jsm_node(""));
        let mut window = inner.clone();
        let mut tab: Snarl<NodeData> = Snarl::new();
        let sp_id = tab.insert_node(egui::pos2(0.0, 0.0), sub_patch(inner));
        let (mut known, mut seen) = (Vec::new(), HashMap::new());
        {
            let mut views = [
                View { prefix: Vec::new(), snarl: &mut tab },
                View { prefix: vec![sp_id], snarl: &mut window },
            ];
            sync_views(&mut views, 0, false, &Mode::Settled, &mut known, &mut seen);
        }

        // Typed in the window; the tab's copy is still blank.
        let text = "S = @Fire\nE = @Ai";
        set_text(&mut window[jsm], text);
        let bump = {
            let mut views = [
                View { prefix: Vec::new(), snarl: &mut tab },
                View { prefix: vec![sp_id], snarl: &mut window },
            ];
            sync_views(&mut views, 0, true, &typing(jsm, text, Some("Ai")), &mut known, &mut seen)
        };
        assert_eq!(bump, [false, true], "only the window is handed on");
        assert_eq!(names(&window[jsm]), ["Fire"]);
        let tab_inner = &tab.get_node(sp_id).unwrap().subpatch.as_ref().unwrap().snarl;
        assert_eq!(names(&tab_inner[jsm]), ["Fire"], "and the tab's copy has it too");
        assert_eq!(jsm_texts(&tab_inner[jsm]), [""], "its text left to the hand-over");
    }
}
