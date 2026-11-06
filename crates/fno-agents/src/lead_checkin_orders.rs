//! The canon-doc block readings a lead beat prints: the handoff orders
//! (the `fno:session` blocks, gated to the holder's knowledge) and the user
//! notes (the `fno:user` block), plus the doc's read check (recall gate).
//! One module owns every `<!-- fno:... -->` block parse of the handoff doc.

use serde_json::Value;
use std::path::Path;

/// The raw text between `<!-- fno:user -->` and its close, marker lines
/// excluded. A missing closing marker is CONTENT, never a parse error: the
/// capture runs to the next marker fence, one of the writer's own section
/// headings, or end of file. Mirrors scripts/lib/canon-doc-marker.sh.
fn extract_user_marker(text: &str) -> Option<String> {
    const WRITER_HEADINGS: [&str; 5] = [
        "## Merge order and why (",
        "## Open decisions awaiting the operator (",
        "## Gaps and open thinking (",
        "## Workarounds in force (",
        "## User notes (",
    ];
    let mut grabbed = String::new();
    let mut inside = false;
    for line in text.lines() {
        if !inside {
            if line.contains("<!-- fno:user -->") {
                inside = true;
            }
            continue;
        }
        if line.contains("<!-- /fno:user -->") {
            return Some(grabbed);
        }
        let fence = line.trim_end();
        if fence.starts_with("<!-- fno:")
            && fence.ends_with(" -->")
            && fence[9..fence.len() - 4].len() > 0
            && fence[9..fence.len() - 4]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Some(grabbed);
        }
        if WRITER_HEADINGS.iter().any(|h| line.starts_with(h)) {
            return Some(grabbed);
        }
        grabbed.push_str(line);
        grabbed.push('\n');
    }
    if inside {
        Some(grabbed)
    } else {
        None
    }
}

/// True when the captured user-block text is only the seed placeholder.
fn is_user_placeholder(text: &str) -> bool {
    const PLACEHOLDER: &str =
        "_(write here; the machine reads this every refresh and never edits it)_";
    let strip = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    !text.is_empty() && strip(text) == strip(PLACEHOLDER)
}

/// The raw text of every `<!-- fno:session -->` block, each entry carrying
/// its `##` heading line and body, marker lines excluded. A block whose
/// close is missing ends at the next `<!-- fno:... -->` fence or `##`
/// heading, mirroring extract_user_marker. The heading travels with the
/// block: "ship A then B" means nothing without "Merge order and why".
fn extract_session_blocks(text: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut heading = String::new();
    let mut capturing = false;
    let mut body = String::new();
    for line in text.lines() {
        if line.starts_with("## ") {
            if capturing {
                blocks.push(std::mem::take(&mut body));
                capturing = false;
            }
            heading = line.to_string();
            continue;
        }
        let fence = line.trim_end();
        if capturing
            && (fence.contains("<!-- /fno:session -->")
                || (fence.starts_with("<!-- fno:") && fence.ends_with(" -->")))
        {
            blocks.push(std::mem::take(&mut body));
            capturing = false;
            continue;
        }
        if !capturing && line.contains("<!-- fno:session -->") {
            capturing = true;
            if !heading.is_empty() {
                body.push_str(&heading);
                body.push('\n');
            }
            continue;
        }
        if capturing {
            body.push_str(line);
            body.push('\n');
        }
    }
    if capturing {
        blocks.push(body);
    }
    blocks
}

/// True when the block text is only one of the writer's seed defaults
/// (hooks/precompact-canon-doc.sh DEFAULT_*): those carry no orders, and
/// printing them every handoff would train the reader to skip the section.
fn is_session_placeholder(text: &str) -> bool {
    const PLACEHOLDERS: [&str; 4] = [
        "_Merge order and the reason for it. Nothing external knows this. The session fills it at full context._",
        "_Open decisions awaiting the operator. Nothing external knows this. The session fills it at full context._",
        "_Gaps and open thinking only this team holds. Nothing external knows this. The session fills it at full context._",
        "_Workarounds in force only this team is running. Nothing external knows this. The session fills it at full context._",
    ];
    let strip = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let stripped = strip(text);
    PLACEHOLDERS.iter().any(|p| stripped == strip(p))
}

/// A stable change marker for the printed text across beats and processes.
fn session_fingerprint(text: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish().to_string()
}

/// The session blocks this beat prints, gated to the holder's knowledge: a
/// holder's first beat always prints; a repeat beat prints only when the
/// blocks changed since this holder's last beat. Null on silence: no doc,
/// every block still the writer's placeholder, or an unchanged repeat beat.
/// The state sidecar mirrors recall_gate's `.{name}.recalled` shape; an
/// unreadable identity always prints (fail toward printing, never silent).
pub(crate) fn r_session_orders(
    handoffs_dir: &Path,
    scope: &str,
    holder: Option<&str>,
) -> Result<Value, String> {
    let Ok(doc) = crate::lead_checkin::team_handoff_doc(handoffs_dir, scope) else {
        return Ok(Value::Null);
    };
    let Ok(text) = std::fs::read_to_string(&doc) else {
        return Ok(Value::Null);
    };
    let blocks: Vec<String> = extract_session_blocks(&text)
        .into_iter()
        .filter(|b| !is_session_placeholder(b))
        .collect();
    if blocks.is_empty() {
        return Ok(Value::Null);
    }
    let joined = blocks.join("");
    let fingerprint = session_fingerprint(&joined);
    let name = doc.file_name().and_then(|n| n.to_str()).unwrap_or("canon");
    let sidecar = doc.with_file_name(format!(".{name}.session-beat"));
    let due = match (holder, std::fs::read_to_string(&sidecar)) {
        // No readable holder: this beat cannot be proven a repeat for anyone.
        (None, _) => true,
        (Some(h), Ok(prev)) => {
            let mut lines = prev.lines();
            lines.next() != Some(h) || lines.next() != Some(fingerprint.as_str())
        }
        // Missing or unreadable sidecar: this holder has never seen the doc.
        (Some(_), Err(_)) => true,
    };
    if due {
        if let Some(h) = holder {
            let _ = std::fs::write(&sidecar, format!("{h}\n{fingerprint}\n"));
        }
        return Ok(Value::String(joined));
    }
    Ok(Value::Null)
}

/// The read check. Every canon refresh ends the doc with a fresh `Recall
/// word:` line, so a word this lead has not acknowledged means a compaction
/// or refresh since its last read. Only `--recall <that word>` passes and
/// records it. No doc, or a doc with no word, passes.
pub(crate) fn recall_gate(
    handoffs_dir: &Path,
    scope: &str,
    given: Option<&str>,
) -> Result<(), String> {
    let Ok(doc) = crate::lead_checkin::team_handoff_doc(handoffs_dir, scope) else {
        return Ok(());
    };
    let Ok(text) = std::fs::read_to_string(&doc) else {
        return Ok(());
    };
    let Some(word) = text
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("Recall word: ").map(str::trim))
    else {
        return Ok(());
    };
    let name = doc.file_name().and_then(|n| n.to_str()).unwrap_or("canon");
    let ack = doc.with_file_name(format!(".{name}.recalled"));
    let acked = std::fs::read_to_string(&ack).unwrap_or_default();
    if acked.trim() == word {
        return Ok(());
    }
    if given == Some(word) {
        std::fs::write(&ack, word).map_err(|e| format!("{}: {e}", ack.display()))?;
        return Ok(());
    }
    Err(format!(
        "your canon doc changed since your last read. Feeling amnesia? Your memory is here. Read all of it: {}. Then rerun with --recall <the word on its last line>.",
        doc.display()
    ))
}

pub(crate) fn r_user_notes(handoffs_dir: &Path, scope: &str) -> Result<Value, String> {
    let doc = crate::lead_checkin::team_handoff_doc(handoffs_dir, scope)?;
    let text =
        std::fs::read_to_string(&doc).map_err(|e| format!("{}: unreadable: {e}", doc.display()))?;
    let block = extract_user_marker(&text)
        .ok_or_else(|| "user block marker missing or doc unreadable".to_string())?;
    if is_user_placeholder(&block) {
        return Ok(Value::Null);
    }
    Ok(Value::String(block))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_rows() {
        let doc = "intro\n<!-- fno:user -->\nline one\n<!-- /fno:user -->\ntail\n";
        assert_eq!(extract_user_marker(doc), Some("line one\n".to_string()));

        let doc = "<!-- fno:user -->\nkept\n## Merge order and why (r1)\nnot kept\n";
        assert_eq!(extract_user_marker(doc), Some("kept\n".to_string()));

        let doc = "<!-- fno:status -->\ns\n<!-- fno:user -->\nkept\n<!-- fno:other -->\n";
        assert_eq!(extract_user_marker(doc), Some("kept\n".to_string()));
        assert_eq!(
            extract_user_marker("<!-- fno:user -->\nkept to end"),
            Some("kept to end\n".to_string())
        );
        assert_eq!(extract_user_marker("no marker here"), None);

        assert!(is_user_placeholder(
            "_(write here; the machine reads this every refresh and never edits it)_\n"
        ));
        assert!(!is_user_placeholder("a real note"));
        assert!(!is_user_placeholder(""));

        // Session blocks: heading carried, markers excluded, an unterminated
        // block ends at the next fence or heading.
        let doc = "intro\n## Merge order and why (r1)\n<!-- fno:session -->\nship A then B\n<!-- /fno:session -->\n## Workarounds in force (r1)\n<!-- fno:session -->\nuse the shim\n<!-- /fno:session -->\ntail\n";
        assert_eq!(
            extract_session_blocks(doc),
            vec![
                "## Merge order and why (r1)\nship A then B\n".to_string(),
                "## Workarounds in force (r1)\nuse the shim\n".to_string(),
            ]
        );
        let doc = "## HANDOFF (session)\n<!-- fno:session -->\nbody runs on\n<!-- fno:status -->\n";
        assert_eq!(
            extract_session_blocks(doc),
            vec!["## HANDOFF (session)\nbody runs on\n".to_string()]
        );
        assert!(extract_session_blocks("no markers here").is_empty());
        assert!(is_session_placeholder(
            "_Merge order and the reason for it. Nothing external knows this. The session fills it at full context._\n"
        ));
        assert!(!is_session_placeholder("merge the PR, not main"));
    }

    #[test]
    fn session_orders_print_on_first_beat_and_on_change() {
        let base = std::env::temp_dir().join(format!("fno-x9a65-{}", std::process::id()));
        // A crashed earlier run must not leave a sidecar that silences beat 1.
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("handoffs");
        std::fs::create_dir_all(&dir).unwrap();
        let doc = dir.join("canon-x-test.md");
        let placeholder = "_Merge order and the reason for it. Nothing external knows this. The session fills it at full context._";
        std::fs::write(
            &doc,
            format!(
                "## Merge order and why (r1)\n<!-- fno:session -->\nship A then B\n<!-- /fno:session -->\n## Workarounds in force (r1)\n<!-- fno:session -->\n{placeholder}\n<!-- /fno:session -->\n"
            ),
        )
        .unwrap();
        // Beat 1, fresh holder: the merge order prints; the placeholder block
        // does not.
        let first = r_session_orders(&dir, "x test", Some("lead-a")).unwrap();
        assert_eq!(
            first,
            Value::String("## Merge order and why (r1)\nship A then B\n".to_string())
        );
        // Beat 2, same holder, unchanged blocks: silent.
        assert_eq!(
            r_session_orders(&dir, "x test", Some("lead-a")).unwrap(),
            Value::Null
        );
        // Beat 3, the blocks changed: prints again.
        std::fs::write(
            &doc,
            "## Merge order and why (r2)\n<!-- fno:session -->\nship C only\n<!-- /fno:session -->\n",
        )
        .unwrap();
        assert_eq!(
            r_session_orders(&dir, "x test", Some("lead-a")).unwrap(),
            Value::String("## Merge order and why (r2)\nship C only\n".to_string())
        );
        // Beat 4, a successor holder on the same unchanged doc: prints on its
        // first beat.
        assert_eq!(
            r_session_orders(&dir, "x test", Some("lead-b")).unwrap(),
            Value::String("## Merge order and why (r2)\nship C only\n".to_string())
        );
        // An unreadable identity prints: fail toward printing, never silent.
        assert!(r_session_orders(&dir, "x test", None).unwrap().is_string());
        // No doc for the scope: nothing, and no failure.
        assert_eq!(
            r_session_orders(&dir, "no-such-scope-zz", Some("lead-a")).unwrap(),
            Value::Null
        );
        // Every block back to the writer's placeholder: nothing.
        std::fs::write(
            &doc,
            format!("## Merge order and why (r1)\n<!-- fno:session -->\n{placeholder}\n<!-- /fno:session -->\n"),
        )
        .unwrap();
        assert_eq!(
            r_session_orders(&dir, "x test", Some("lead-a")).unwrap(),
            Value::Null
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The read check: a fresh word refuses (naming the doc) until --recall
    /// names it; a wrong word refuses; the right word passes and is recorded.
    #[test]
    fn recall_gate_refuses_until_the_doc_word_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("20261027-role-fnh-fno.md");
        std::fs::write(&doc, "# Canon doc\n\nRecall word: 1a2b3c4d\n").unwrap();
        let err = recall_gate(dir.path(), "fnh,fno", None).unwrap_err();
        assert!(err.contains(&doc.display().to_string()), "{err}");
        assert!(recall_gate(dir.path(), "fnh,fno", Some("wrong")).is_err());
        assert!(recall_gate(dir.path(), "fnh,fno", Some("1a2b3c4d")).is_ok());
        assert!(recall_gate(dir.path(), "fnh,fno", None).is_ok());
        std::fs::write(&doc, "# Canon doc\n\nRecall word: 99887766\n").unwrap();
        assert!(recall_gate(dir.path(), "fnh,fno", None).is_err());
    }

    #[test]
    fn placeholder_mixed_content_never_reads_placeholder() {
        // A block that carries an order plus the seed sentence is content,
        // not filler: only a whitespace-equal match reads placeholder.
        let mixed = "## Merge order and why (r1)\n_Merge order and the reason for it. Nothing external knows this. The session fills it at full context._\nship A first\n";
        assert!(!is_session_placeholder(mixed));
    }
}
