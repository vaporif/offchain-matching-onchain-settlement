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

    let solc = Command::new("which")
        .arg("solc")
        .output()
        .expect("which solc failed");
    let solc_path = String::from_utf8(solc.stdout).unwrap();
    let solc_path = solc_path.trim();

    let status = Command::new("forge")
        .args(["build"])
        .env("FOUNDRY_SOLC", solc_path)
        .current_dir(&contracts_dir)
        .status()
        .expect("forge build failed — is foundry installed?");
    assert!(status.success(), "forge build failed");
}
