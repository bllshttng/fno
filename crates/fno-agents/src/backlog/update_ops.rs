//! The `field=value` / `field+=value` / `field-=value` operator grammar for
//! `fno backlog update`. Pure syntax and pure edits: nothing here reads the
//! graph store, so the module stays unit-testable without a fixture. Slice 2
//! grows `OP_FIELDS`; the grammar itself is closed.

/// The operator a token carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Set,
    Add,
    Remove,
}

/// One parsed operator token: the field, the operator, the value after the
/// first `=`, and the token as typed (refusals quote it verbatim).
#[derive(Debug, Clone)]
pub struct FieldOp {
    pub field: String,
    pub op: Op,
    pub value: String,
    pub token: String,
}

/// The two field shapes the operators act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    List,
}

/// Slice 1's five fields: text appends and list add/remove.
const OP_FIELDS: &[(&str, FieldKind)] = &[
    ("details", FieldKind::Text),
    ("dispatch_brief", FieldKind::Text),
    ("tags", FieldKind::List),
    ("blocked_by", FieldKind::List),
    ("related", FieldKind::List),
];

/// Parse one argv token as an operator: no leading `-`, a field name shaped
/// `[a-z][a-z0-9_]*`, and an optional `+`/`-` directly before the FIRST `=`
/// (so `details=a=b` sets the value `a=b`). A node id never contains `=`, so
/// an id is never read as an operator. `None` means not operator-shaped and
/// the caller falls through to its positional checks.
pub fn parse_op_token(raw: &str) -> Option<FieldOp> {
    if raw.starts_with('-') {
        return None;
    }
    let (head, value) = raw.split_once('=')?;
    let (field, op) = match head.strip_suffix('+') {
        Some(f) => (f, Op::Add),
        None => match head.strip_suffix('-') {
            Some(f) => (f, Op::Remove),
            None => (head, Op::Set),
        },
    };
    let shaped = !field.is_empty()
        && field.starts_with(|c: char| c.is_ascii_lowercase())
        && field
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !shaped {
        return None;
    }
    Some(FieldOp {
        field: field.to_string(),
        op,
        value: value.to_string(),
        token: raw.to_string(),
    })
}

/// Validate one parsed op against the field table: an unknown field refuses,
/// a remove on text refuses, and an empty add to text refuses.
pub fn check(op: &FieldOp) -> Result<FieldKind, String> {
    let Some((_, kind)) = OP_FIELDS.iter().find(|(f, _)| *f == op.field) else {
        let names: Vec<&str> = OP_FIELDS.iter().map(|(f, _)| *f).collect();
        return Err(format!(
            "Error: unknown field '{}'. Operator fields: {}. Other fields keep their \
             flags for now: fno backlog update --help",
            op.field,
            names.join(", ")
        ));
    };
    match op.op {
        Op::Remove if *kind == FieldKind::Text => Err(format!(
            "Error: {} is text. It takes = and += only.",
            op.field
        )),
        Op::Add if *kind == FieldKind::Text && op.value.trim().is_empty() => {
            Err(format!("Error: {}+= needs text.", op.field))
        }
        _ => Ok(*kind),
    }
}

/// The append join: the trimmed `existing`, two newlines, then `add`; `add`
/// alone when there is nothing to join; `None` when the trimmed existing
/// text already ends with the trimmed `add` (the retry-safe no-op). The
/// literal `null` is data here; only `=` treats it as clear.
pub fn append_text(existing: Option<&str>, add: &str) -> Option<String> {
    match existing {
        Some(text) if !text.trim().is_empty() => {
            if text.trim().ends_with(add.trim()) {
                None
            } else {
                Some(format!("{}\n\n{}", text.trim_end(), add))
            }
        }
        _ => Some(add.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_splits_on_the_first_equals() {
        let op = parse_op_token("details+=a=b").expect("operator");
        assert_eq!(op.field, "details");
        assert_eq!(op.op, Op::Add);
        assert_eq!(op.value, "a=b");
        assert_eq!(op.token, "details+=a=b");
    }

    #[test]
    fn non_operator_shapes_return_none() {
        assert!(parse_op_token("--title=T").is_none());
        assert!(parse_op_token("-d=x").is_none());
        assert!(parse_op_token("plain").is_none());
        assert!(parse_op_token("x-aaaa1111").is_none());
        assert!(parse_op_token("Bad_Field=1").is_none());
        assert!(parse_op_token("=value").is_none());
    }
}
