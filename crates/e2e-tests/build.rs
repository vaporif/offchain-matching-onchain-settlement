use std::process::Command;

fn main() {
    let contracts_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("contracts");

    println!(
        "cargo::rerun-if-changed={}",
        contracts_dir.join("src").display()
    );
    println!(
        "cargo::rerun-if-changed={}",
        contracts_dir.join("foundry.toml").display()
    );
    println!(
        "cargo::rerun-if-changed={}",
        contracts_dir.join("package.json").display()
    );

    let output = Command::new("bun")
        .args(["install", "--frozen-lockfile"])
        .current_dir(&contracts_dir)
        .output()
        .expect("bun install failed — is bun installed?");
    assert!(
        output.status.success(),
        "bun install failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut cmd = Command::new("forge");
    cmd.args(["build"]).current_dir(&contracts_dir);

    if let Ok(output) = Command::new("which").arg("solc").output() {
        let path = String::from_utf8_lossy(&output.stdout);
        let path = path.trim();
        if !path.is_empty() && output.status.success() {
            cmd.env("FOUNDRY_SOLC", path);
        }
    }

    let output = cmd
        .output()
        .expect("forge build failed — is foundry installed?");
    assert!(
        output.status.success(),
        "forge build failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
