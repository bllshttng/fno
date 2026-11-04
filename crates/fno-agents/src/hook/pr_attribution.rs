//! `fno-agents hook pr-attribution <command>`: is this Bash command one bare
//! `gh pr create`? `hooks/claim-heartbeat.sh` binds the PR URL a raw create
//! prints only when the answer is `bare`. It prints one word: `bare` (the
//! command starts with `gh pr create` and holds no shell operator),
//! `compound` (a create sits inside a longer command), `other` (no create),
//! or `invalid` (the command does not lex). Always exits 0.

fn is_operator(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| ";&|()`\n".contains(c))
}

pub(crate) fn attribution(command: &str) -> &'static str {
    // A backslash-newline joins two lines into one command line.
    let joined = command.replace("\\\r\n", "").replace("\\\n", "");
    let Some(tokens) = super::lead_guard::lex(&joined) else {
        return "invalid";
    };
    let creates: Vec<usize> = tokens
        .windows(3)
        .enumerate()
        .filter(|(_, w)| w[0] == "gh" && w[1] == "pr" && w[2] == "create")
        .map(|(i, _)| i)
        .collect();
    // Substitution bodies come back after a `\n` token, so a create inside
    // `$( )` or backticks reads as compound, never bare.
    if creates == [0] && !tokens.iter().any(|t| is_operator(t)) {
        "bare"
    } else if !creates.is_empty() {
        "compound"
    } else {
        "other"
    }
}

pub fn run(args: &[String]) -> i32 {
    println!(
        "{}",
        attribution(args.first().map(String::as_str).unwrap_or(""))
    );
    0
}

#[cfg(test)]
mod tests {
    use super::attribution;

    #[test]
    fn only_a_lone_create_is_bare() {
        assert_eq!(attribution("gh pr create --title 'a; b' --body x"), "bare");
        assert_eq!(attribution("gh pr create \\\n  --fill"), "bare");
        assert_eq!(attribution("git push && gh pr create --fill"), "compound");
        assert_eq!(attribution("echo $(gh pr create --fill)"), "compound");
        assert_eq!(attribution("gh pr view 3"), "other");
        assert_eq!(attribution("gh pr create --title 'open"), "invalid");
    }
}
