use crate::Service;
use drover_core::config::Config;
use drover_core::events::append;
use drover_core::{shed, Ctx};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

const RETRY_SECS: u64 = 5;

impl Service {
    pub(crate) fn shed(self: Arc<Self>, w: &shed::Wire) {
        loop {
            let ctx = Ctx {
                now: (self.clock)(),
                ..self.ctx.clone()
            };
            let why = Config::load(&ctx.config).and_then(|cfg| {
                shed::follow(w, shed::seen(&ctx), None, |e| match e {
                    None => self.stream_is("shed", None),
                    Some(e) => {
                        let c = Ctx {
                            now: (self.clock)(),
                            ..ctx.clone()
                        };
                        let _ = shed::log(&c, &cfg, &e);
                        let _ = std::fs::write(c.state.with_file_name("shed.seq"), e.seq.to_string());
                    }
                })
            });
            let why = why
                .err()
                .map_or("shed: the event stream closed".into(), |e| format!("{e:#}"));
            self.stream_is("shed", Some(why));
            std::thread::sleep(Duration::from_secs(RETRY_SECS));
        }
    }

    fn stream_is(&self, runtime: &'static str, down: Option<String>) {
        let mut s = self.streams.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if s.insert(runtime, down.is_none()) == Some(down.is_none()) {
            return;
        }
        let d = match down {
            None => json!({"runtime": runtime, "state": "live"}),
            Some(why) => json!({"runtime": runtime, "state": "down", "why": why}),
        };
        let _ = append(&self.ctx.state, &self.event("lane-stream", d));
    }
}
