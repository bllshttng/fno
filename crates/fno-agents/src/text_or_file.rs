//! One seam for "long text from an argument or a file", the twin of
//! `fno/text_or_file.py`. The refusal texts must stay byte-equal to it; each
//! caller keeps its own exit code and stream.

use std::io::Read;

/// The text from whichever form was given; `None` when neither. Refuses both
/// at once, reads `-` as stdin, and refuses an unreadable file. `what` names
/// the text in the refusal (`the prompt`, `the details`).
pub fn read_text_arg(
    inline: Option<&str>,
    path: Option<&str>,
    what: &str,
) -> Result<Option<String>, String> {
    let Some(path) = path else {
        return Ok(inline.map(str::to_string));
    };
    if inline.is_some() {
        return Err(format!(
            "error: provide {what} once - inline or as a file, not both"
        ));
    }
    if path == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("error: cannot read -: {e}"))?;
        return Ok(Some(buf));
    }
    std::fs::read_to_string(path)
        .map(Some)
        .map_err(|e| format!("error: cannot read {path}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::read_text_arg;

    #[test]
    fn one_form_reads_and_both_or_unreadable_refuse() {
        assert_eq!(
            read_text_arg(Some("hi"), None, "the prompt"),
            Ok(Some("hi".into()))
        );
        assert_eq!(read_text_arg(None, None, "the prompt"), Ok(None));
        assert_eq!(
            read_text_arg(Some("hi"), Some("f"), "the prompt"),
            Err("error: provide the prompt once - inline or as a file, not both".into())
        );
        let dir = std::env::temp_dir().join(format!("text-or-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("seed.txt");
        std::fs::write(&file, "from a file").unwrap();
        let path = file.to_string_lossy().into_owned();
        assert_eq!(
            read_text_arg(None, Some(&path), "the prompt"),
            Ok(Some("from a file".into()))
        );
        let missing = dir.join("absent.txt").to_string_lossy().into_owned();
        let err = read_text_arg(None, Some(&missing), "the prompt").unwrap_err();
        assert!(
            err.starts_with(&format!("error: cannot read {missing}: ")),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
