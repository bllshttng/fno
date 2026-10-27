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
