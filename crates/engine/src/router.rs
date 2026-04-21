use flexinput_core::Signal;

/// Determines how physical input signals are distributed each tick.
///
/// Normal: all signals flow into the graph engine as usual.
/// Overlay: nominated signals are diverted to the overlay UI for navigation
///          and parameter tweaking; the remainder still reach the engine.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum RouterMode {
    #[default]
    Normal,
    Overlay,
}

pub struct InputRouter {
    pub mode: RouterMode,
    /// Pin IDs (device_id::pin_id) captured by the overlay in Overlay mode.
    pub overlay_captures: Vec<String>,
}

impl InputRouter {
    pub fn new() -> Self {
        Self { mode: RouterMode::Normal, overlay_captures: vec![] }
    }

    /// Returns (graph_signals, overlay_signals) for a batch of raw device events.
    pub fn route(
        &self,
        events: Vec<(String, String, Signal)>,
    ) -> (Vec<(String, String, Signal)>, Vec<(String, String, Signal)>) {
        if self.mode == RouterMode::Normal {
            return (events, vec![]);
        }

        let mut graph = Vec::new();
        let mut overlay = Vec::new();
        for ev in events {
            let key = format!("{}::{}", ev.0, ev.1);
            if self.overlay_captures.contains(&key) {
                overlay.push(ev);
            } else {
                graph.push(ev);
            }
        }
        (graph, overlay)
    }
}

impl Default for InputRouter {
    fn default() -> Self {
        Self::new()
    }
}
