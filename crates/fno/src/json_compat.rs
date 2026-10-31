pub fn py_parse_error(raw: &str, error: &serde_json::Error) -> String {
    let text = error.to_string();
    let Some((message, rest)) = text.split_once(" at line ") else {
        return text;
    };
    let Some((line, column)) = rest.split_once(" column ") else {
        return text;
    };
    let (Ok(line), Ok(column)) = (line.parse::<usize>(), column.parse::<usize>()) else {
        return text;
    };
    let line_start = if line <= 1 {
        0
    } else {
        raw.match_indices('\n')
            .nth(line - 2)
            .map_or(0, |(i, _)| i + 1)
    };
    let mut position = (line_start + column.saturating_sub(1)).min(raw.len());
    let phrase = match message {
        "expected value" => "Expecting value",
        "EOF while parsing a value" => {
            position = raw.len();
            "Expecting value"
        }
        "expected `:`" => "Expecting ':' delimiter",
        "expected `,` or `}`" | "expected `,` or `]`" => "Expecting ',' delimiter",
        "key must be a string" => "Expecting property name enclosed in double quotes",
        "EOF while parsing an object" => {
            position = raw.len();
            if raw.trim_end().ends_with(['{', ',']) {
                "Expecting property name enclosed in double quotes"
            } else {
                "Expecting ',' delimiter"
            }
        }
        "EOF while parsing a list" => {
            position = raw.len();
            if raw.trim_end().ends_with(['[', ',']) {
                "Expecting value"
            } else {
                "Expecting ',' delimiter"
            }
        }
        "trailing characters" => "Extra data",
        "trailing comma" => {
            position = raw[..position].rfind(',').unwrap_or(position);
            if raw.trim_end().ends_with('}') {
                "Illegal trailing comma before end of object"
            } else {
                "Illegal trailing comma before end of array"
            }
        }
        "invalid escape" => {
            position = raw[..position].rfind('\\').unwrap_or(position);
            "Invalid \\escape"
        }
        m if m.starts_with("control character (") => "Invalid control character at",
        "EOF while parsing a string" => {
            let mut open = None;
            let mut escaped = false;
            for (i, ch) in raw.char_indices() {
                if escaped {
                    escaped = false;
                    continue;
                }
                if ch == '\\' && open.is_some() {
                    escaped = true;
                    continue;
                }
                if ch == '"' {
                    open = if open.is_some() { None } else { Some(i) };
                }
            }
            position = open.unwrap_or(position);
            "Unterminated string starting at"
        }
        _ => return text,
    };
    while !raw.is_char_boundary(position) {
        position -= 1;
    }
    let before = &raw[..position];
    let line = before.bytes().filter(|b| *b == b'\n').count() + 1;
    let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    let char_position = before.chars().count();
    format!("{phrase}: line {line} column {column} (char {char_position})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_json_diagnostics_preserve_positions_and_categories() {
        let cases = [
            (
                "{",
                "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)",
            ),
            ("{\"a\":}", "Expecting value: line 1 column 6 (char 5)"),
            (
                "{\"a\":1",
                "Expecting ',' delimiter: line 1 column 7 (char 6)",
            ),
            (
                "{\"a\": \"",
                "Unterminated string starting at: line 1 column 7 (char 6)",
            ),
            (
                "{\"a\":1,}",
                "Illegal trailing comma before end of object: line 1 column 7 (char 6)",
            ),
            ("{\"é\": }", "Expecting value: line 1 column 7 (char 6)"),
            ("[] trailing", "Extra data: line 1 column 4 (char 3)"),
            ("[", "Expecting value: line 1 column 2 (char 1)"),
            (
                "[1,]",
                "Illegal trailing comma before end of array: line 1 column 3 (char 2)",
            ),
            ("{\n \"a\": }", "Expecting value: line 2 column 7 (char 8)"),
            ("\"bad\\q\"", "Invalid \\escape: line 1 column 5 (char 4)"),
        ];
        for (raw, expected) in cases {
            let error = serde_json::from_str::<serde_json::Value>(raw).unwrap_err();
            assert_eq!(py_parse_error(raw, &error), expected, "{raw}");
        }
    }
}
