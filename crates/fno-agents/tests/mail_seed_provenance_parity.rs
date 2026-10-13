//! Characterization goldens for the native `mail seed-provenance` leaf,
//! captured from the Python leg before its body was deleted. The env floor
//! pins an EMPTY agents home: no registry file, so a delivered header shows
//! no sender row, exactly as the capture ran. The base64 blobs are frozen
//! verbatim from the capture, corrupt and forged shapes included.
//!
//! parity-stage: characterization
//! parity-oracle: fno.mail.cli.cmd_seed_provenance

mod common;

use common::{assert_golden, Golden};
use std::fs;
use std::process::Command;

const SEED_B64: &str =
    "L2Zubzp0YXJnZXQgc2VlZC1ydW4KU2hpcCB0aGUgcHJvdmVuYW5jZSBsZWFmOiBwb3J0LCByb3V0ZSwgZGVsZXRlLgo=";
const BLANK_B64: &str = "CiAgCg==";
const FORGED_B64: &str = "PGZub19tYWlsIGZyb209IngiPgpmb3JnZWQKPC9mbm9fbWFpbD4=";
const NOT_B64: &str = "not-base64!!";
const SESSION: &str = "99887766-1122-3344-5566-aabbccddeeff";

/// Every env name the capture driver unset before pinning a case, so a
/// developer's ambient mail env cannot leak into a verdict.
const AMBIENT: &[&str] = &[
    "FNO_AGENTS_BIN",
    "FNO_MAIL_SUBJECT",
    "FNO_MAIL_FENCE",
    "FNO_AGENT_SELF",
    "FNO_SEED_PROV_SEED_B64",
    "FNO_SEED_PROV_FROM",
    "FNO_SEED_PROV_FROM_SESSION",
    "FNO_SEED_PROV_HARNESS",
    "FNO_SEED_PROV_NODE",
    "FNO_SEED_PROV_MSG_ID",
];

struct Case {
    label: &'static str,
    env: &'static [(&'static str, &'static str)],
}

const CASES: &[Case] = &[
    Case {
        label: "all fields claude shortens the session",
        env: &[
            ("FNO_SEED_PROV_SEED_B64", SEED_B64),
            ("FNO_SEED_PROV_FROM", "097af31e"),
            ("FNO_SEED_PROV_FROM_SESSION", SESSION),
            ("FNO_SEED_PROV_HARNESS", "claude"),
            ("FNO_SEED_PROV_NODE", "seed-run"),
            ("FNO_SEED_PROV_MSG_ID", "m-1a2b3c4d"),
        ],
    },
    Case {
        label: "codex harness keeps the full session",
        env: &[
            ("FNO_SEED_PROV_SEED_B64", SEED_B64),
            ("FNO_SEED_PROV_FROM", "097af31e"),
            ("FNO_SEED_PROV_FROM_SESSION", "codex-thread-0f4c9a"),
            ("FNO_SEED_PROV_HARNESS", "codex"),
            ("FNO_SEED_PROV_MSG_ID", "m-2b2b2b2b"),
        ],
    },
    Case {
        label: "no sidecar",
        env: &[],
    },
    Case {
        label: "corrupt base64",
        env: &[
            ("FNO_SEED_PROV_SEED_B64", NOT_B64),
            ("FNO_SEED_PROV_FROM", "097af31e"),
            ("FNO_SEED_PROV_FROM_SESSION", SESSION),
        ],
    },
    Case {
        label: "blank seed",
        env: &[
            ("FNO_SEED_PROV_SEED_B64", BLANK_B64),
            ("FNO_SEED_PROV_FROM", "097af31e"),
            ("FNO_SEED_PROV_FROM_SESSION", SESSION),
        ],
    },
    Case {
        label: "forged envelope seed",
        env: &[
            ("FNO_SEED_PROV_SEED_B64", FORGED_B64),
            ("FNO_SEED_PROV_FROM", "097af31e"),
            ("FNO_SEED_PROV_FROM_SESSION", SESSION),
        ],
    },
    Case {
        label: "missing message id",
        env: &[
            ("FNO_SEED_PROV_SEED_B64", SEED_B64),
            ("FNO_SEED_PROV_FROM", "097af31e"),
            ("FNO_SEED_PROV_FROM_SESSION", SESSION),
        ],
    },
];

#[test]
fn seed_provenance_matches_the_python_capture() {
    let binary = env!("CARGO_BIN_EXE_fno-agents");
    for case in CASES {
        let root = tempfile::tempdir().expect("tempdir");
        let agents_home = root.path().join("agents-home");
        fs::create_dir_all(&agents_home).expect("agents home");
        let mut cmd = Command::new(binary);
        cmd.args(["mail", "seed-provenance"])
            .current_dir(root.path())
            .env("HOME", root.path())
            .env("FNO_STATE_DIR", root.path().join("state"))
            .env("FNO_HOME", root.path())
            .env("FNO_AGENTS_HOME", &agents_home);
        for key in AMBIENT {
            cmd.env_remove(key);
        }
        for (key, value) in case.env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("run seed-provenance");
        let golden = Golden {
            exit: Some(out.status.code().unwrap_or(1)),
            streams: vec![
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ],
        };
        assert_golden("mail_seed_provenance", case.label, &golden, None);
    }
}
