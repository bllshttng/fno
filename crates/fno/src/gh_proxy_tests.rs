//! Contracts for the gh proxy installer, replacing the deleted Python tests:
//! the shim names fno-agents by absolute path and reads back as a shim, a
//! foreign gh is backed up once, and a delegate that is the shim refuses.

use crate::gh_proxy::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[test]
fn wrapper_reads_back_as_a_shim_with_or_without_quoting() {
    let dir = tempfile::tempdir().unwrap();
    for binary in ["/opt/fno/bin/fno-agents", "/Users/a b/it's/fno-agents"] {
        let shim = dir.path().join("gh");
        std::fs::write(&shim, wrapper(Path::new(binary))).unwrap();
        assert!(is_wrapper(&shim), "{binary}");
    }
    assert_eq!(shell_quote("/a/b"), "/a/b");
    assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
    let foreign = dir.path().join("other");
    std::fs::write(&foreign, "#!/bin/sh\nexec /usr/bin/gh \"$@\"\n").unwrap();
    assert!(!is_wrapper(&foreign));
}

#[test]
fn install_backs_up_a_foreign_gh_once_and_drops_the_legacy_link() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("github-cli");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("gh"), "real gh bytes").unwrap();
    std::fs::write(root.join("fno-gh-proxy"), "old").unwrap();
    let real = dir.path().join("real-gh");
    std::fs::write(&real, "#!/bin/sh\n").unwrap();
    let binary = Path::new("/opt/fno/bin/fno-agents");

    install_shim(&root, binary, &real).unwrap();
    let shim = root.join("gh");
    assert_eq!(std::fs::read_to_string(&shim).unwrap(), wrapper(binary));
    assert!(std::fs::metadata(&shim).unwrap().permissions().mode() & 0o111 != 0);
    assert_eq!(
        std::fs::read_to_string(root.join("gh.pre-fno")).unwrap(),
        "real gh bytes"
    );
    assert!(!root.join("fno-gh-proxy").exists());

    // A rerun over its own shim backs nothing up again.
    install_shim(&root, binary, &real).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("gh.pre-fno")).unwrap(),
        "real gh bytes"
    );
}

#[test]
fn install_refuses_a_delegate_that_is_the_shim() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::write(root.join("gh"), "x").unwrap();
    let err = install_shim(&root, Path::new("/opt/fno-agents"), &root.join("gh")).unwrap_err();
    assert!(err.contains("points back to the Footnote proxy"), "{err}");
}

#[test]
fn in_temp_judges_every_temp_root_in_both_spellings() {
    // A fixture under the OS temp root reads as temp even while a scrubbed
    // driver's TMPDIR floors to /tmp, and the raw /private spelling holds.
    let fixture = std::env::temp_dir().join("fno-in-temp-fixture/gh");
    assert!(in_temp(&fixture));
    assert!(in_temp(Path::new("/private/var/folders/ab/T/fno-501/github-cli")));
    // A sibling sharing the prefix string is not under it, and the base
    // itself is not strictly under.
    assert!(!in_temp(Path::new("/tmpx/fno/gh")));
    assert!(!in_temp(Path::new("/tmp")));
    assert!(!in_temp(Path::new("/opt/fno/bin/fno-agents")));
}

#[test]
fn is_executable_refuses_a_dead_or_unrunnable_binary() {
    let dir = tempfile::tempdir().unwrap();
    // fs::write sets no execute bit: the swept-override shape.
    let dead = dir.path().join("swept/fno-agents");
    std::fs::create_dir_all(dead.parent().unwrap()).unwrap();
    std::fs::write(&dead, b"payload").unwrap();
    assert!(!is_executable(&dead));
    std::fs::remove_dir_all(dead.parent().unwrap()).unwrap();
    // Deleted after resolve: the TOCTOU the guard in ensure_proxy reads at
    // install time, so a durable shim never lands pointing here.
    assert!(!is_executable(&dead));
    let live = dir.path().join("live");
    std::fs::write(&live, "#!/bin/sh\n").unwrap();
    let mut perm = std::fs::metadata(&live).unwrap().permissions();
    #[allow(clippy::precedence)]
    perm.set_mode(perm.mode() | 0o111);
    std::fs::set_permissions(&live, perm).unwrap();
    assert!(is_executable(&live));
}
