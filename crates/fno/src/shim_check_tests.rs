//! Contracts for the shim sweep, replacing the deleted Python tests: which
//! links are defects, which absences count, and that a repair relinks to the
//! durable copy and leaves the re-scan clean.

use super::*;
use std::os::unix::fs::{symlink, PermissionsExt};

/// The front-door shim, named through the contract it checks.
const FNO: &str = REQUIRED_SHIMS[0];

struct Scratch {
    _dir: tempfile::TempDir,
    roots: Roots,
}

fn scratch() -> Scratch {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(dir.path());
    let mk = |rel: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    };
    let roots = Roots {
        bin_dir: mk("bin"),
        self_bin: Some(mk("venv/bin")),
        temp_root: mk("tmp"),
        uv_bin: mk("uv/bin"),
        cargo_bin: mk("cargo/bin"),
    };
    Scratch { _dir: dir, roots }
}

fn exe(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn dangling_and_temp_links_relink_to_the_durable_copy() {
    let s = scratch();
    let r = &s.roots;
    exe(&r.self_bin.as_ref().unwrap().join(FNO));
    exe(&r.uv_bin.join("fno-py"));
    exe(&r.cargo_bin.join("fno-gh-proxy"));
    exe(&r.temp_root.join("old/fno-py"));
    symlink(r.temp_root.join("gone/fno"), r.bin_dir.join(FNO)).unwrap();
    symlink(r.temp_root.join("old/fno-py"), r.bin_dir.join("fno-py")).unwrap();
    // Not ours: a broken third-party link is no finding.
    symlink(r.temp_root.join("gone/gh"), r.bin_dir.join("gh")).unwrap();
    // A link into this front door's own bin is healthy.
    exe(&r.self_bin.as_ref().unwrap().join("fno-agents"));
    symlink(
        r.self_bin.as_ref().unwrap().join("fno-agents"),
        r.bin_dir.join("fno-agents"),
    )
    .unwrap();

    let found: Vec<(String, &str)> = scan(r).into_iter().map(|d| (d.name, d.problem)).collect();
    assert_eq!(
        found,
        vec![
            ("fno".to_string(), "dangling"),
            ("fno-py".to_string(), "temp-resolving"),
            ("fno-gh-proxy".to_string(), "absent"),
        ]
    );
    assert_eq!(run_roots(r, false), 1, "a scan alone reports and fails");
    assert_eq!(run_roots(r, true), 0, "the repair leaves a clean re-scan");
    let link = |n: &str| std::fs::read_link(r.bin_dir.join(n)).unwrap();
    assert_eq!(link("fno"), r.self_bin.as_ref().unwrap().join(FNO));
    assert_eq!(link("fno-py"), r.uv_bin.join("fno-py"));
    assert_eq!(link("fno-gh-proxy"), r.cargo_bin.join("fno-gh-proxy"));
    assert_eq!(link("gh"), r.temp_root.join("gone/gh"));
}

#[test]
fn an_absence_counts_only_with_a_linkable_durable_copy() {
    let s = scratch();
    let r = &s.roots;
    // A pure-Python install ships no fno binary: nothing to link, no defect.
    assert!(scan(r).is_empty());

    // A durable copy under the temp root would relink into a temp-resolving
    // defect, so its absence is not reported either.
    let in_temp = Roots {
        uv_bin: r.temp_root.join("uv/bin"),
        self_bin: None,
        bin_dir: r.bin_dir.clone(),
        temp_root: r.temp_root.clone(),
        cargo_bin: r.cargo_bin.clone(),
    };
    exe(&in_temp.uv_bin.join(FNO));
    assert!(scan(&in_temp).is_empty());

    // An unrepairable defect survives the repair and names itself.
    symlink(r.temp_root.join("gone/fno-x"), r.bin_dir.join("fno-x")).unwrap();
    let defects = scan(r);
    assert_eq!(defects.len(), 1);
    assert_eq!(repair(&defects), vec!["fno-x: no durable copy".to_string()]);
    assert_eq!(run_roots(r, true), 1);
}
