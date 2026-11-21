use serde_json::Value;
use std::path::Path;

fn is_zed_gpl(package: &Value) -> bool {
    package["source"]
        .as_str()
        .is_some_and(|source| source.starts_with("git+https://github.com/zed-industries/zed"))
        && package["license"]
            .as_str()
            .is_some_and(|license| license.contains("GPL"))
}

#[test]
fn only_yard_ui_depends_on_zeds_gpl_crates() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root exists");
    let output = std::process::Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--offline"])
        .current_dir(repository)
        .output()
        .expect("cargo metadata runs");
    assert!(
        output.status.success(),
        "cargo metadata --offline failed; it needs every crate of every platform \
         in Cargo's cache, which `cargo fetch` downloads: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).expect("cargo metadata is JSON");
    let packages = metadata["packages"].as_array().unwrap();
    let nodes = metadata["resolve"]["nodes"].as_array().unwrap();
    let package = |id: &Value| {
        packages
            .iter()
            .find(|package| package["id"] == *id)
            .expect("resolved package exists")
    };
    let mut ui_depends_on_theme = false;
    for member in metadata["workspace_members"].as_array().unwrap() {
        let member_package = package(member);
        let name = member_package["name"].as_str().unwrap();
        let node = nodes
            .iter()
            .find(|node| node["id"] == *member)
            .expect("workspace member is resolved");
        for dependency in node["deps"].as_array().unwrap() {
            let dependency_package = package(&dependency["pkg"]);
            if name == "yard_ui" {
                if dependency_package["name"] == "theme" {
                    assert!(
                        is_zed_gpl(dependency_package),
                        "theme must be detected as a GPL crate of Zed"
                    );
                    ui_depends_on_theme = true;
                }
            } else {
                assert!(
                    !is_zed_gpl(dependency_package),
                    "{name} depends on {}: only yard_ui may depend on Zed's GPL crates",
                    dependency_package["name"].as_str().unwrap()
                );
            }
        }
    }
    assert!(ui_depends_on_theme, "yard_ui must depend on theme");
}
