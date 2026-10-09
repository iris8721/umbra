// The command-line driver. Stress-test finding 37: a script's errors cite
// its path, not `[string "..."]`.

use std::process::Command;

#[test]
fn script_errors_cite_the_file_path() {
    let dir = std::env::temp_dir().join(format!("umbra-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("boom.umbra");
    let shown = path.display().to_string();
    let run = || {
        let out = Command::new(env!("CARGO_BIN_EXE_umbra")).arg(&path).output().unwrap();
        assert!(!out.status.success());
        String::from_utf8_lossy(&out.stderr).into_owned()
    };

    std::fs::write(&path, "fn boom() { error(\"bang\") }\nboom()\n").unwrap();
    assert_eq!(run().trim_end(), format!("umbra: {shown}:1: bang"));
    std::fs::write(&path, "let x = 1\nlet = 2\n").unwrap();
    assert!(run().starts_with(&format!("umbra: {shown}: line 2: expected identifier")));
    // -e has no file to name.
    let out = Command::new(env!("CARGO_BIN_EXE_umbra")).args(["-e", "error(\"x\")"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stderr).trim_end(), "umbra: [string \"...\"]:1: x");

    std::fs::remove_dir_all(&dir).unwrap();
}
