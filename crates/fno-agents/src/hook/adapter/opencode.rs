//! The opencode hook payload: the claude-shaped JSON the plugin
//! (`cli/src/fno/setup/assets/opencode/footnote.js`) builds from opencode's
//! plugin events. `model` arrives as `provider/model`, opencode's own
//! `--model` spelling. A permission prompt arrives as a Notification.

use super::HookEvent;
use serde_json::Value;

pub(crate) fn normalize(event: &str, payload: &Value) -> HookEvent {
    super::plugin_shaped("opencode", event, payload)
}
