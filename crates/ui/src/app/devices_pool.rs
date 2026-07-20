//! Shared virtual-device pool: which ids the open tabs still reference,
//! and creating / retiring pool entries to match.

use super::*;

/// Collect every `device_id` string referenced by a `device.sink` node in
/// the given snarl that targets a virtual device (id starts with
/// `"virtual."`). Used to drive shared-pool reconciliation and the
/// active-tab id filter.
pub(crate) fn snarl_virtual_device_ids(snarl: &Snarl<NodeData>) -> Vec<String> {
    snarl
        .nodes_ids_data()
        .filter_map(|(_, n)| {
            let node = &n.value;
            if node.module_id == "device.sink" {
                node.params
                    .get("device_id")
                    .and_then(|v| v.as_str())
                    .filter(|id| id.starts_with("virtual."))
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// True if a `gilrs:<kind>:<inst>` PHYSICAL device id is one of FlexInput's own
/// emulated HIDMaestro pads — the input enumerator tags those with a `v`-prefixed
/// instance (`gilrs:dualsense:v0`). Used to filter the loopback devices out of
/// the physical list deterministically (vs the old plug-order heuristic).
pub(crate) fn is_own_virtual_gilrs_id(gilrs_id: &str) -> bool {
    gilrs_id
        .rsplit(':')
        .next()
        .map(|inst| inst.starts_with('v'))
        .unwrap_or(false)
}

/// Map an own-virtual loopback id (`gilrs:<kind>:v<N>`) to the matching virtual
/// pool device: its `kind_prefix` (e.g. `virtual.hm.xinput`) and the `N`th
/// own-virtual of that kind. Returns `None` for a real physical id. Used by the
/// rumble "ping" to inject into the virtual's feedback rather than a backend
/// (which has no motor for our own virtual). The `v<N>` index matches pool order
/// because the read-side tags own-virtuals (`gilrs_device_id`) in the same
/// enumeration order the pool lists them.
pub(crate) fn own_virtual_pool_target(gilrs_id: &str) -> Option<(&'static str, usize)> {
    let mut parts = gilrs_id.split(':');
    if parts.next()? != "gilrs" {
        return None;
    }
    let slug = parts.next()?;
    let inst = parts.next()?;
    let n: usize = inst.strip_prefix('v')?.parse().ok()?; // own-virtuals only
    let kind_prefix = match slug {
        "xinput" => "virtual.hm.xinput",
        "ds4" => "virtual.hm.ds4",
        "dualsense" => "virtual.hm.dualsense",
        _ => return None,
    };
    Some((kind_prefix, n))
}


/// Insert virtual devices into the shared pool for every id in
/// `needed_ids` that doesn't already exist. Pre-existing devices are
/// reused — never duplicated. Devices the pool has but `needed_ids`
/// doesn't list are left alone (pruning is a separate operation).
/// Enqueue async `Create` ops for any needed device id that isn't already in the
/// pool and isn't already in flight. Non-blocking: the worker builds each device
/// (blocking helper IPC happens there) and the UI installs it into `pool` when
/// the result arrives (see `drain_device_op_results`). `pending` tracks in-flight
/// ids so we never enqueue a duplicate before the first lands.
pub(crate) fn reconcile_shared_devices(
    pool: &Mutex<Vec<Box<dyn VirtualDevice>>>,
    pending: &mut HashSet<String>,
    failed: &HashSet<String>,
    tx: &std::sync::mpsc::Sender<crate::device_ops::DeviceOp>,
    needed_ids: &[String],
) {
    let existing: HashSet<String> = {
        let pool = pool.lock().unwrap();
        pool.iter().map(|d| d.id().to_string()).collect()
    };
    for id in needed_ids {
        // Skip what's already built, in flight, or known-failed (failed ids are
        // only retried after the node is removed + re-added — see drain).
        if existing.contains(id) || pending.contains(id) || failed.contains(id) {
            continue;
        }
        // Only enqueue ids we can actually build (known kinds).
        if try_create_virtual_device_known(id) {
            pending.insert(id.clone());
            let _ = tx.send(crate::device_ops::DeviceOp::Create { device_id: id.clone() });
        }
    }
}

/// True if `id` names a known virtual device kind (mirrors the kind/instance
/// split in `try_create_virtual_device` without building anything).
pub(crate) fn try_create_virtual_device_known(id: &str) -> bool {
    let kind_id = match id.rfind('.') {
        Some(dot) => match id[dot + 1..].parse::<usize>() {
            Ok(_) => &id[..dot],
            Err(_) => id,
        },
        None => id,
    };
    flexinput_virtual::available_device_kinds()
        .iter()
        .any(|k| k.kind_id == kind_id)
}

/// Remove devices from the shared pool whose id is not referenced by any open
/// tab's canvas, and **return the removed `Box`es** so the caller can drop them
/// off the UI thread (a HIDMaestro device's `Drop` calls `helper::destroy`,
/// which blocks). Called after closing a tab / reloading a workspace. The pool
/// lock is acquired internally and released before returning.
pub(crate) fn take_unreferenced_devices(
    pool: &Mutex<Vec<Box<dyn VirtualDevice>>>,
    tabs: &[PatchTab],
) -> Vec<Box<dyn VirtualDevice>> {
    let mut keep: HashSet<String> = HashSet::new();
    for tab in tabs {
        for id in snarl_virtual_device_ids(&tab.canvas.snarl) {
            keep.insert(id);
        }
    }
    let mut pool = pool.lock().unwrap();
    let mut removed = Vec::new();
    let mut i = 0;
    while i < pool.len() {
        if keep.contains(pool[i].id()) {
            i += 1;
        } else {
            removed.push(pool.remove(i));
        }
    }
    removed
}

/// Take every device no longer referenced by the open tabs out of the pool and
/// hand it to the worker for off-thread teardown (so `helper::destroy` doesn't
/// block the UI). Also clears any matching in-flight `pending` create. No-op
/// when nothing needs removing.
pub(crate) fn prune_devices_async(
    pool: &Mutex<Vec<Box<dyn VirtualDevice>>>,
    pending: &mut HashSet<String>,
    tx: &std::sync::mpsc::Sender<crate::device_ops::DeviceOp>,
    tabs: &[PatchTab],
) {
    for dev in take_unreferenced_devices(pool, tabs) {
        let device_id = dev.id().to_string();
        pending.remove(&device_id);
        let _ = tx.send(crate::device_ops::DeviceOp::Destroy { device_id, device: dev });
    }
}
