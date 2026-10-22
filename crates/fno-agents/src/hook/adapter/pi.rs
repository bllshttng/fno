//! The pi hook payload: the claude-shaped JSON the extension
//! (`cli/src/fno/setup/assets/pi/footnote.ts`) builds from pi's extension
//! events. pi exposes no permission-prompt event, so no Notification arrives.

use super::HookEvent;
use serde_json::Value;

pub(crate) fn normalize(event: &str, payload: &Value) -> HookEvent {
    super::plugin_shaped("pi", event, payload)
}
