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

    let status = Command::new("bun")
        .args(["install", "--frozen-lockfile"])
        .current_dir(&contracts_dir)
        .status()
        .expect("bun install failed — is bun installed?");
    assert!(status.success(), "bun install failed");

    let mut cmd = Command::new("forge");
    cmd.args(["build"]).current_dir(&contracts_dir);

    if let Ok(output) = Command::new("which").arg("solc").output() {
        let path = String::from_utf8_lossy(&output.stdout);
        let path = path.trim();
        if !path.is_empty() && output.status.success() {
            cmd.env("FOUNDRY_SOLC", path);
        }
    }

    let status = cmd
        .status()
        .expect("forge build failed — is foundry installed?");
    assert!(status.success(), "forge build failed");
}
