pub fn repo_identity_from_remote_url(url: &str) -> Option<String> {
    let mut s = url.trim().trim_end_matches('/');
    if s.is_empty() {
        return None;
    }
    if let Some(idx) = s.find("://") {
        s = &s[idx + 3..];
    }
    if let Some(idx) = s.find('@') {
        s = &s[idx + 1..];
    }
    // Split host from path on the first `/` or `:` (scp form uses `:`).
    let (host_port, path) = match s.find(['/', ':']) {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => return None,
    };
    // `ssh://host:22/o/r` leaves a numeric port glued to the host; drop it, but
    // never drop a non-numeric suffix that is part of the host.
    let host = match host_port.split_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => h,
        _ => host_port,
    };
    // An scp remote may still carry a leading port segment (`host:22/o/r`).
    let path = path.trim_start_matches('/');
    let path = match path.split_once('/') {
        Some((first, rest)) if !first.is_empty() && first.chars().all(|c| c.is_ascii_digit()) => {
            rest
        }
        _ => path,
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty() || path.contains('\\') {
        return None;
    }
    let segments: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if segments.len() < 2 || segments.iter().any(|p| *p == "." || *p == "..") {
        return None;
    }
    Some(format!("{}/{}", host, segments.join("/")).to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_normalizes_remote_forms() {
        assert_eq!(
            repo_identity_from_remote_url("git@github.com:Org/Repo.git").as_deref(),
            Some("github.com/org/repo")
        );
        assert_eq!(
            repo_identity_from_remote_url("ssh://host:22/org/repo.git").as_deref(),
            Some("host/org/repo")
        );
        assert_eq!(repo_identity_from_remote_url("local"), None);
    }
}
