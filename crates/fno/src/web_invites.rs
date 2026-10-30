//! The command page's invite gate. A bridge that sits behind `tailscale
//! serve` binds loopback, and tailscale adds `Tailscale-User-Login` to each
//! proxied request. That header proves a login only on a loopback bind: on a
//! wide bind any client can send it, so a wide bind has no identity at all.
//!
//! `[[web.invites]]` rows (`email`, `scope`, `role`) say who may do what. The
//! decision here is pure, so every refusal is a table test with no listener.

use std::path::Path;

/// Rank order is the grant order: a higher role does everything a lower one does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Role {
    Viewer,
    /// Parsed so a commenter row is legal. No route needs it yet, so it grants
    /// nothing above viewer.
    Commenter,
    Editor,
    Owner,
}

impl Role {
    fn parse(s: &str) -> Option<Role> {
        match s.trim().to_ascii_lowercase().as_str() {
            "viewer" => Some(Role::Viewer),
            "commenter" => Some(Role::Commenter),
            "editor" => Some(Role::Editor),
            "owner" => Some(Role::Owner),
            _ => None,
        }
    }
}

/// One invite row. `scope` is `*` or a node id; a node id covers that node and
/// every node under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Invite {
    pub(crate) email: String,
    pub(crate) scope: String,
    pub(crate) role: Role,
}

/// Why a request was refused: the status the route returns and one line the
/// page shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) code: u16,
    pub(crate) text: String,
}

fn refuse(code: u16, text: &str) -> Refusal {
    Refusal {
        code,
        text: text.to_string(),
    }
}

/// The `[[web.invites]]` rows from the same config order every mux reader
/// uses. `None` when no row exists, so an install with no invites changes
/// nothing. Some rows may be malformed: a row with a missing field or an
/// unknown role is dropped and grants nothing, but its presence still turns
/// the gate on.
pub(crate) fn read_invites(cwd: &Path) -> Option<Vec<Invite>> {
    let raw = crate::digest_overlay::resolve_config_key(cwd, &|path| {
        let table = std::fs::read_to_string(path)
            .ok()?
            .parse::<toml::Table>()
            .ok()?;
        let rows = table.get("web")?.as_table()?.get("invites")?.as_array()?;
        (!rows.is_empty())
            .then(|| serde_json::to_string(rows).ok())
            .flatten()
    })?;
    Some(parse_invites(&raw))
}

fn parse_invites(raw: &str) -> Vec<Invite> {
    let rows: Vec<serde_json::Value> = serde_json::from_str(raw).unwrap_or_default();
    rows.iter()
        .filter_map(|row| {
            let field = |k: &str| row.get(k).and_then(serde_json::Value::as_str);
            let email = field("email")?.trim().to_ascii_lowercase();
            let scope = field("scope").unwrap_or("*").trim().to_string();
            let role = Role::parse(field("role")?)?;
            (!email.is_empty() && !scope.is_empty()).then_some(Invite { email, scope, role })
        })
        .collect()
}

/// What a request carries: the facts the decision reads.
pub(crate) struct Caller<'a> {
    /// `Tailscale-User-Login`, as sent.
    pub(crate) login: Option<&'a str>,
    /// The bridge is bound to loopback.
    pub(crate) loopback_bind: bool,
    /// The request's Host names loopback, so it did not come through the proxy.
    pub(crate) loopback_host: bool,
}

/// Allow with the name to stamp on the work, or refuse with the reason.
/// `chain` is the node and its ancestors; `None` means the route has no node,
/// so any scope counts.
pub(crate) fn decide(
    caller: &Caller<'_>,
    invites: Option<&[Invite]>,
    chain: Option<&[String]>,
    needed: Role,
) -> Result<String, Refusal> {
    let login = caller.login.map(|l| l.trim().to_ascii_lowercase());
    let login = login.filter(|l| !l.is_empty());
    // The user at their own machine: a direct loopback request carries no
    // proxy login. They own every scope.
    if caller.loopback_bind && caller.loopback_host && login.is_none() {
        return Ok("local".to_string());
    }
    let Some(invites) = invites else {
        // No invite list: reads keep today's token-only rule, and nothing
        // else is open to a request that came through the proxy.
        return if needed == Role::Viewer {
            Ok("token".to_string())
        } else {
            Err(refuse(
                403,
                "answering from here needs web.invites in config: no invite list is set",
            ))
        };
    };
    if !caller.loopback_bind {
        return Err(refuse(
            403,
            "web.invites is set, so the bridge must bind loopback behind tailscale serve",
        ));
    }
    let Some(login) = login else {
        return Err(refuse(
            403,
            "no login on the request: it did not come through tailscale serve",
        ));
    };
    let mine: Vec<&Invite> = invites.iter().filter(|i| i.email == login).collect();
    if mine.is_empty() {
        return Err(refuse(403, "this login is not on the invite list"));
    }
    let covers =
        |inv: &Invite| inv.scope == "*" || chain.is_none_or(|c| c.iter().any(|n| *n == inv.scope));
    let scoped: Vec<&&Invite> = mine.iter().filter(|i| covers(i)).collect();
    if scoped.is_empty() {
        return Err(refuse(403, "this invite does not cover that node"));
    }
    if scoped.iter().any(|i| i.role >= needed) {
        Ok(login)
    } else {
        Err(refuse(403, "this invite's role does not allow that"))
    }
}

#[cfg(test)]
mod web_invites_tests {
    use super::*;

    fn inv(email: &str, scope: &str, role: Role) -> Invite {
        Invite {
            email: email.into(),
            scope: scope.into(),
            role,
        }
    }

    fn tailnet(login: &str) -> Caller<'_> {
        Caller {
            login: Some(login),
            loopback_bind: true,
            loopback_host: false,
        }
    }

    fn chain(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn web_invites_parse_drops_bad_rows_and_lowercases() {
        let rows = parse_invites(
            r#"[{"email":"Ann@Example.com","scope":"x-1","role":"Editor"},
                {"email":"bob@example.com","role":"boss"},
                {"scope":"*","role":"viewer"},
                {"email":"cy@example.com","role":"viewer"}]"#,
        );
        assert_eq!(
            rows,
            vec![
                inv("ann@example.com", "x-1", Role::Editor),
                inv("cy@example.com", "*", Role::Viewer),
            ]
        );
    }

    fn web_invites_a_scoped_editor_answers_inside_the_scope_only() {
        let list = [inv("ann@example.com", "x-1", Role::Editor)];
        let ok = decide(
            &tailnet("ann@example.com"),
            Some(&list),
            Some(&chain(&["x-9", "x-1"])),
            Role::Editor,
        );
        assert_eq!(ok, Ok("ann@example.com".to_string()));
        let out = decide(
            &tailnet("ann@example.com"),
            Some(&list),
            Some(&chain(&["x-2", "x-3"])),
            Role::Editor,
        );
        assert_eq!(out.unwrap_err().code, 403);
    }

    fn web_invites_a_low_role_an_unlisted_login_and_a_wide_bind_all_refuse() {
        let list = [
            inv("view@example.com", "*", Role::Viewer),
            inv("talk@example.com", "*", Role::Commenter),
        ];
        let c = chain(&["x-1"]);
        for who in ["view@example.com", "talk@example.com"] {
            let r = decide(&tailnet(who), Some(&list), Some(&c), Role::Editor);
            assert_eq!(r.unwrap_err().code, 403, "{who}");
        }
        let r = decide(&tailnet("eve@example.com"), Some(&list), None, Role::Viewer);
        assert_eq!(r.unwrap_err().code, 403);
        let wide = Caller {
            login: Some("view@example.com"),
            loopback_bind: false,
            loopback_host: false,
        };
        assert_eq!(
            decide(&wide, Some(&list), None, Role::Viewer)
                .unwrap_err()
                .code,
            403
        );
        // A viewer reads: no node, any scope.
        assert!(decide(
            &tailnet("view@example.com"),
            Some(&list),
            None,
            Role::Viewer
        )
        .is_ok());
    }

    fn web_invites_no_list_keeps_token_reads_and_refuses_a_tailnet_answer() {
        let c = chain(&["x-1"]);
        let remote = tailnet("ann@example.com");
        assert_eq!(
            decide(&remote, None, None, Role::Viewer),
            Ok("token".into())
        );
        let r = decide(&remote, None, Some(&c), Role::Editor).unwrap_err();
        assert_eq!(r.code, 403);
        assert!(r.text.contains("web.invites"), "{}", r.text);
        // The user on their own machine still answers.
        let local = Caller {
            login: None,
            loopback_bind: true,
            loopback_host: true,
        };
        assert_eq!(
            decide(&local, None, Some(&c), Role::Editor),
            Ok("local".into())
        );
        // The local pass needs a loopback bind: a wide bind has no identity.
        let wide_local = Caller {
            login: None,
            loopback_bind: false,
            loopback_host: true,
        };
        assert!(decide(&wide_local, None, Some(&c), Role::Editor).is_err());
    }

    fn web_invites_configured_list_still_lets_the_local_user_in() {
        let list = [inv("ann@example.com", "x-1", Role::Editor)];
        let local = Caller {
            login: None,
            loopback_bind: true,
            loopback_host: true,
        };
        assert_eq!(
            decide(&local, Some(&list), Some(&chain(&["x-7"])), Role::Editor),
            Ok("local".into())
        );
    }

    #[test]
    fn web_invites_decide_table() {
        web_invites_parse_drops_bad_rows_and_lowercases();
        web_invites_a_scoped_editor_answers_inside_the_scope_only();
        web_invites_a_low_role_an_unlisted_login_and_a_wide_bind_all_refuse();
    }

    #[test]
    fn web_invites_no_list_and_local_user() {
        web_invites_no_list_keeps_token_reads_and_refuses_a_tailnet_answer();
        web_invites_configured_list_still_lets_the_local_user_in();
    }
}
