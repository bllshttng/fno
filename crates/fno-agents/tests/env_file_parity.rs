//! Parity for the deleted Python leg `cli/src/fno/env_file.py`, retired onto
//! `fno_agents::provider_key::read_var_from_env_file` by the notify-family
//! port. The goldens carry the Python leg's return for each fixture, JSON
//! dumped (so a missing key reads `null`, a found value a JSON string).
//!
//! parity-stage: characterization
//! parity-oracle: cli/src/fno/env_file.py
//!
//! Capture deviation: the mission freeze barred local cargo runs, so the
//! goldens were captured by driving the Python leg directly with a script
//! instead of the harness's FNO_CAPTURE_GOLDEN=1 path. The oracle file is
//! gone, so capture mode now refuses, by design - these bytes are final.

mod common;

use common::{assert_golden, Golden};

fn dumped(got: Option<String>) -> Golden {
    let text = match &got {
        Some(s) => serde_json::to_string(s).unwrap(),
        None => "null".to_string(),
    };
    Golden {
        exit: Some(0),
        streams: vec![text],
    }
}

#[test]
fn env_file_cases_match_the_captured_goldens() {
    let tmp = std::env::temp_dir().join(format!("env-file-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let fixture = tmp.join("fixture.env");
    let read = |body: &str| {
        std::fs::write(&fixture, body).unwrap();
        dumped(fno_agents::provider_key::read_var_from_env_file(
            &fixture.display().to_string(),
            "KEY",
        ))
    };

    assert_golden(
        "env_file",
        "env file plain value",
        &read("KEY=value\n"),
        None,
    );
    assert_golden(
        "env_file",
        "env file export prefix",
        &read("export KEY=value\n"),
        None,
    );
    assert_golden(
        "env_file",
        "env file double quoted",
        &read("KEY=\"quoted value\"\n"),
        None,
    );
    assert_golden(
        "env_file",
        "env file single quoted",
        &read("KEY='single quoted'\n"),
        None,
    );
    assert_golden("env_file", "env file missing key", &read("OTHER=1\n"), None);
    assert_golden(
        "env_file",
        "env file comment lines",
        &read("# KEY=no\n\nKEY=yes\n"),
        None,
    );
    assert_golden(
        "env_file",
        "env file spaces around equals",
        &read("KEY = spaced \n"),
        None,
    );
    assert_golden(
        "env_file",
        "env file empty value reads none",
        &read("KEY=\n"),
        None,
    );
    assert_golden(
        "env_file",
        "env file value keeps inner quotes",
        &read("KEY=a\"b\n"),
        None,
    );
    assert_golden(
        "env_file",
        "env file export with spaces",
        &read("  export   KEY=deep\n"),
        None,
    );
    assert_golden(
        "env_file",
        "env file no equals sign",
        &read("JUST_A_NAME\nKEY=found\n"),
        None,
    );
    assert_golden(
        "env_file",
        "env file missing file",
        &dumped(fno_agents::provider_key::read_var_from_env_file(
            &tmp.join("absent.env").display().to_string(),
            "KEY",
        )),
        None,
    );
    let _ = std::fs::remove_dir_all(&tmp);
}
