pub const BREVITY_MARKER: &str = "<fno_relay_compression>";
pub const BREVITY_END_MARKER: &str = "</fno_relay_compression>";
pub const BREVITY_INSTRUCTION: &str = "Keep reports and handoffs at 80 words or fewer unless this payload requires a longer artifact or exact output schema. Think fully; write only the requested result, essential reason, and next action. Drop filler, pleasantries, hedges, repeated context, and articles where clear. Fragments work. Keep technical terms, commands, errors, numbers, units, negation, and code blocks exact. Put long detail in durable artifacts when available; return a path or link.";

pub fn enrich_spawn_payload(message: &str) -> String {
    let block = format!("{BREVITY_MARKER}\n{BREVITY_INSTRUCTION}\n{BREVITY_END_MARKER}");
    if message.is_empty() || message.contains(&block) {
        return message.to_owned();
    }
    format!("{message}\n\n{block}")
}

/// The full payload entry: brevity guidance once, then the fleet-law block
/// last, nearest the worker's eye, read for a worker seated at `cwd`. The
/// vec names what was delivered, for the spawn receipt; empty means no
/// active fleet constraint rode the seed.
pub fn finalize_spawn_payload_at(message: &str, cwd: &std::path::Path) -> (String, Vec<String>) {
    let enriched = enrich_spawn_payload(message);
    crate::fleet_law::append_fleet_laws_at(&enriched, cwd)
}

#[cfg(test)]
mod tests {
    use super::{enrich_spawn_payload, BREVITY_END_MARKER, BREVITY_INSTRUCTION, BREVITY_MARKER};

    #[test]
    fn empty_and_already_enriched_payloads_are_unchanged() {
        assert_eq!(enrich_spawn_payload(""), "");
        let enriched = enrich_spawn_payload("report");
        assert_eq!(enrich_spawn_payload(&enriched), enriched);
    }

    #[test]
    fn runtime_constants_match_the_shared_cross_language_fixture() {
        const FIXTURE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schemas/spawn-brevity.json"
        ));
        let fixture: serde_json::Value = serde_json::from_str(FIXTURE).expect("valid fixture");

        assert_eq!(fixture["marker"], BREVITY_MARKER);
        assert_eq!(fixture["end_marker"], BREVITY_END_MARKER);
        assert_eq!(fixture["instruction"], BREVITY_INSTRUCTION);
    }
}
