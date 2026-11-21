use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

fn repository() -> PathBuf {
    // Not canonicalized: on Windows that gives a `\\?\` path, and the paths
    // of `cargo metadata`, which are compared with this one, have none.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the repository root exists")
        .to_path_buf()
}

/// Cargo resolves workspace globs, crate names, and dependency aliases for us.
fn metadata() -> Value {
    let output = std::process::Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--offline",
            "--no-deps",
        ])
        .current_dir(repository())
        .output()
        .expect("rule 5: cargo metadata runs");
    assert!(
        output.status.success(),
        "rule 5: cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("cargo metadata is JSON")
}

fn staged_crates() -> Vec<(String, PathBuf)> {
    let metadata = metadata();
    metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|package| {
            metadata["workspace_members"]
                .as_array()
                .unwrap()
                .contains(&package["id"])
        })
        .map(|package| {
            (
                package["name"].as_str().unwrap().to_owned(),
                Path::new(package["manifest_path"].as_str().unwrap())
                    .parent()
                    .unwrap()
                    .to_path_buf(),
            )
        })
        .filter(|(_, folder)| !folder.starts_with(repository().join("examples")))
        .collect()
}

fn root_files(name: &str, folder: &Path) -> Vec<PathBuf> {
    [
        "main.rs".to_owned(),
        "lib.rs".to_owned(),
        format!("{}.rs", name.replace('-', "_")),
        "dev.rs".to_owned(),
    ]
    .into_iter()
    .map(|name| folder.join("src").join(name))
    .filter(|path| path.is_file())
    .collect()
}

fn files_under(folder: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(&path, files);
        } else {
            files.push(path);
        }
    }
}

/// Test files have no stage.
fn is_test_file(path: &Path) -> bool {
    let name = path.file_name().unwrap().to_string_lossy();
    name == "tests.rs"
        || name == "testing.rs"
        || name.ends_with("_tests.rs")
        || path.components().any(|part| part.as_os_str() == "tests")
}

fn relative(path: &Path) -> String {
    path.strip_prefix(repository())
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/")
}

/// FNV-1a, 64 bit, of a file's bytes, with each CRLF read as LF, so that a
/// Windows checkout, where git writes CRLF, gives the same fingerprint.
fn fingerprint(path: &Path) -> String {
    let bytes = fs::read(path).expect("the file can be read");
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            continue;
        }
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The parts outside `dev/`, as `<crate>/<module>`, each with its files.
fn parts_outside_dev() -> BTreeMap<String, BTreeMap<String, String>> {
    let mut parts: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (name, folder) in staged_crates() {
        let source = folder.join("src");
        let roots = root_files(&name, &folder);
        let entries = fs::read_dir(&source).expect("the crate has a src folder");
        for entry in entries.flatten() {
            let path = entry.path();
            let module = path.file_stem().unwrap().to_string_lossy().into_owned();
            if roots.contains(&path) || module == "dev" || is_test_file(&path) {
                continue;
            }
            let mut files = Vec::new();
            let module_file = source.join(format!("{module}.rs"));
            if module_file.is_file() {
                files.push(module_file);
            }
            files_under(&source.join(&module), &mut files);
            let part = format!("{name}/{module}");
            let listed = parts.entry(part).or_default();
            for file in files.iter().filter(|file| !is_test_file(file)) {
                listed.insert(relative(file), fingerprint(file));
            }
        }
    }
    parts.retain(|_, files| !files.is_empty());
    parts
}

fn ledger_file() -> PathBuf {
    repository().join("production.json")
}

fn ledger() -> BTreeMap<String, BTreeMap<String, Value>> {
    let text = fs::read_to_string(ledger_file()).expect("production.json exists");
    serde_json::from_str(&text).expect("production.json is a JSON object")
}

fn today() -> String {
    // TODO(stamp-date-portable): Stamping uses the Unix date command and fails on Windows. Replace it with a portable date source when making stamping work on Windows.
    let output = std::process::Command::new("date")
        .arg("+%F")
        .output()
        .expect("date runs");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn what_is_outside_dev_is_in_production() {
    let parts = parts_outside_dev();
    let mut ledger = ledger();
    if let Ok(names) = std::env::var("YARD_PROMOTE") {
        for name in names
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            let files = parts.get(name).unwrap_or_else(|| {
                panic!("rule 9: production.json: {name} is not outside `dev/`: move its module out first")
            });
            ledger.insert(
                name.to_owned(),
                BTreeMap::from([
                    ("files".to_owned(), json!(files)),
                    ("promoted".to_owned(), json!(today())),
                ]),
            );
            println!("stamped {name}: {} files", files.len());
        }
        let text = serde_json::to_string_pretty(&ledger).unwrap();
        fs::write(ledger_file(), text + "\n").expect("production.json can be written");
    }
    let mut problems = Vec::new();
    for (part, files) in &parts {
        let Some(entry) = ledger.get(part) else {
            problems.push(format!(
                "rule 8: {part}: {} is outside `dev/` and is not listed in production.json",
                files.keys().next().unwrap()
            ));
            continue;
        };
        let listed: BTreeMap<String, String> =
            serde_json::from_value(entry["files"].clone()).unwrap_or_default();
        let names: BTreeSet<&String> = files.keys().chain(listed.keys()).collect();
        for file in names {
            match (files.get(file), listed.get(file)) {
                (Some(now), Some(then)) if now == then => {}
                (Some(_), Some(_)) => {
                    problems.push(format!("rule 8: {part}: {file} fingerprint differs"))
                }
                (Some(_), None) => problems.push(format!("rule 8: {part}: {file} is new")),
                (None, _) => problems.push(format!("rule 8: {part}: {file} is gone")),
            }
        }
    }
    for part in ledger.keys() {
        if !parts.contains_key(part) {
            let files = ledger[part]["files"]
                .as_object()
                .expect("rule 7: files is an object");
            for file in files.keys() {
                problems.push(format!("rule 8: {part}: {file} is gone (part is gone)"));
            }
            if files.is_empty() {
                problems.push(format!("rule 8: {part}: part is gone (production.json)"));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "production code is not as the user approved it. Show the user what \
         changed; when the user approves, stamp the part again:\n  {}",
        problems.join("\n  ")
    );
}

/// Keep strings whole and discard comments so layout cannot hide root items.
fn tokens(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut result = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
        } else if chars[i..].starts_with(&['/', '/']) {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if chars[i..].starts_with(&['/', '*']) {
            i += 2;
            let mut depth = 1;
            while i < chars.len() && depth > 0 {
                if chars[i..].starts_with(&['/', '*']) {
                    depth += 1;
                    i += 2;
                } else if chars[i..].starts_with(&['*', '/']) {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else {
            let start = i;
            i += 1;
            if chars[start] == '"' {
                while i < chars.len() {
                    if chars[i] == '\\' {
                        i += 2;
                    } else if chars[i] == '"' {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
            } else if chars[start].is_alphanumeric() || chars[start] == '_' {
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
            }
            result.push(chars[start..i.min(chars.len())].iter().collect());
        }
    }
    result
}

#[test]
fn the_dev_feature_is_written_only_in_root_files() {
    for (name, folder) in staged_crates() {
        let roots = root_files(&name, &folder);
        let mut files = Vec::new();
        files_under(&folder.join("src"), &mut files);
        for file in files {
            if roots.contains(&file)
                || is_test_file(&file)
                || file.extension().is_none_or(|ext| ext != "rs")
            {
                continue;
            }
            let words = tokens(&fs::read_to_string(&file).unwrap());
            assert!(
                !words
                    .windows(3)
                    .any(|words| words == ["feature", "=", "\"dev\""]),
                "rule 4: {}: feature = \"dev\" belongs only in root files",
                relative(&file)
            );
        }
    }
}

/// Consume a balanced attribute, use group, or main body.
fn through_group(words: &[String], start: usize, open: &str, close: &str) -> usize {
    let mut depth = 0;
    for (index, word) in words.iter().enumerate().skip(start) {
        if word == open {
            depth += 1;
        }
        if word == close {
            depth -= 1;
            if depth == 0 {
                return index + 1;
            }
        }
    }
    words.len()
}

#[test]
fn root_files_hold_no_code_of_their_own() {
    for (name, folder) in staged_crates() {
        for file in root_files(&name, &folder) {
            let words = tokens(&fs::read_to_string(&file).unwrap());
            let mut i = 0;
            while i < words.len() {
                let start = i;
                if words[i] == "#" {
                    i += 1;
                    if words.get(i).is_some_and(|word| word == "!") {
                        i += 1;
                    }
                    assert_eq!(
                        words.get(i).map(String::as_str),
                        Some("["),
                        "rule 3: {}: invalid attribute",
                        relative(&file)
                    );
                    i = through_group(&words, i, "[", "]");
                    continue;
                }
                let mut visibility = "";
                if words[i] == "pub" {
                    visibility = "pub";
                    i += 1;
                    if words.get(i).is_some_and(|word| word == "(") {
                        assert!(
                            words
                                .get(i..i + 3)
                                .is_some_and(|group| group == ["(", "crate", ")"]),
                            "rule 3: {}: unsupported visibility",
                            relative(&file)
                        );
                        visibility = "pub(crate)";
                        i += 3;
                    }
                }
                let item = words.get(i).map(String::as_str).unwrap_or("");
                if item == "use" || (item == "mod" && visibility != "pub(crate)") {
                    i += 1;
                    while i < words.len() && words[i] != ";" {
                        if words[i] == "{" && item == "use" {
                            i = through_group(&words, i, "{", "}");
                        } else {
                            assert_ne!(
                                words[i],
                                "{",
                                "rule 3: {}: inline module holds code",
                                relative(&file)
                            );
                            i += 1;
                        }
                    }
                    assert!(
                        i < words.len(),
                        "rule 3: {}: declaration needs a semicolon",
                        relative(&file)
                    );
                    i += 1;
                } else if item == "fn"
                    && visibility.is_empty()
                    && file.file_name().unwrap() != "dev.rs"
                    && words.get(i + 1).is_some_and(|word| word == "main")
                {
                    while i < words.len() && words[i] != "{" {
                        i += 1;
                    }
                    i = through_group(&words, i, "{", "}");
                } else {
                    panic!(
                        "rule 3: {}: root holds code of its own near `{}` (token {})",
                        relative(&file),
                        words[start],
                        start + 1
                    );
                }
            }
        }
    }
}

#[test]
fn every_crate_passes_the_dev_feature_on() {
    let metadata = metadata();
    let packages: Vec<_> = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|package| {
            metadata["workspace_members"]
                .as_array()
                .unwrap()
                .contains(&package["id"])
                && !Path::new(package["manifest_path"].as_str().unwrap())
                    .starts_with(repository().join("examples"))
        })
        .collect();
    for package in &packages {
        let location = package["manifest_path"].as_str().unwrap();
        let features = &package["features"];
        let dev = features["dev"]
            .as_array()
            .unwrap_or_else(|| panic!("rule 5: {location}: missing dev feature"));
        assert!(
            features["default"]
                .as_array()
                .is_some_and(|values| values.contains(&json!("dev"))),
            "rule 5: {location}: dev must be on by default"
        );
        for dependency in package["dependencies"].as_array().unwrap() {
            if !packages.iter().any(|other| {
                other["name"] == dependency["name"]
                    && dependency["path"].as_str()
                        == Path::new(other["manifest_path"].as_str().unwrap())
                            .parent()
                            .and_then(Path::to_str)
            }) {
                continue;
            }
            let name = dependency["rename"]
                .as_str()
                .unwrap_or_else(|| dependency["name"].as_str().unwrap());
            assert_eq!(
                dependency["uses_default_features"], false,
                "rule 5: {location}: {name} needs default-features = false"
            );
            assert!(
                dev.contains(&json!(format!("{name}/dev"))),
                "rule 5: {location}: dev must include {name}/dev"
            );
        }
    }
}
