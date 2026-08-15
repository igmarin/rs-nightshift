use std::{fs, path::Path};
struct Declaration {
    source: &'static str,
    value: String,
    version: (u64, u64),
}
#[test]
fn toolchain_declarations_are_consistent() {
    if let Err(error) = check_declarations(Path::new(env!("CARGO_MANIFEST_DIR"))) {
        panic!("{error}");
    }
}
fn check_declarations(root: &Path) -> Result<(), String> {
    let sources = [
        ("Cargo.toml", "rust-version"),
        ("rust-toolchain.toml", "channel"),
        (".mise.toml", "rust"),
        ("dist-workspace.toml", "rust-toolchain-version"),
    ];
    let mut declarations = sources
        .into_iter()
        .map(|(source, key)| {
            let contents = read_source(root, source)?;
            parse_declaration(source, assignment(&contents, source, key)?)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let ci = ".github/workflows/ci.yml";
    let values = workflow_toolchains(&read_source(root, ci)?)?;
    declarations.extend(
        values
            .into_iter()
            .map(|value| parse_declaration(ci, value))
            .collect::<Result<Vec<_>, _>>()?,
    );
    // Compare major.minor so Cargo.toml's patchless MSRV matches pinned toolchains.
    let expected = declarations[0].version;
    if declarations
        .iter()
        .any(|declaration| declaration.version != expected)
    {
        let details = declarations
            .iter()
            .map(|declaration| {
                format!(
                    "  {}: {} (major.minor {}.{})",
                    declaration.source,
                    declaration.value,
                    declaration.version.0,
                    declaration.version.1
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!(
            "Rust toolchain declarations disagree (patch components are intentionally ignored):\n\
             {details}"
        ));
    }
    Ok(())
}
fn read_source(root: &Path, source: &'static str) -> Result<String, String> {
    fs::read_to_string(root.join(source))
        .map_err(|error| format!("{source}: could not read source: {error}"))
}
fn assignment(contents: &str, source: &'static str, key: &str) -> Result<String, String> {
    let mut values = contents.lines().filter_map(|line| {
        let line = line.trim();
        if line.starts_with('#') || !line.starts_with(key) {
            return None;
        }
        line[key.len()..]
            .trim_start()
            .strip_prefix('=')
            .map(|value| value.trim().to_owned())
    });
    let value = values
        .next()
        .ok_or_else(|| format!("{source}: missing {key} declaration"))?;
    if values.next().is_some() {
        return Err(format!("{source}: multiple {key} declarations"));
    }
    parse_scalar(source, key, &value)
}
fn workflow_toolchains(contents: &str) -> Result<Vec<String>, String> {
    let values = contents
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            (!line.starts_with('#'))
                .then(|| {
                    line.strip_prefix("toolchain:")
                        .map(|value| value.trim().to_owned())
                })
                .flatten()
        })
        .collect::<Vec<_>>();
    if values.is_empty() {
        return Err(
            ".github/workflows/ci.yml: no toolchain: inputs found; refusing a vacuous pass"
                .to_owned(),
        );
    }
    values
        .into_iter()
        .map(|value| parse_scalar(".github/workflows/ci.yml", "toolchain", &value))
        .collect()
}
fn parse_scalar(source: &'static str, key: &str, value: &str) -> Result<String, String> {
    let value = value.split('#').next().unwrap_or_default().trim();
    let value = if value.starts_with('"') || value.starts_with('\'') {
        let quote = value.as_bytes()[0] as char;
        if !value.ends_with(quote) || value.len() < 2 {
            return Err(format!("{source}: malformed {key} declaration: {value}"));
        }
        &value[1..value.len() - 1]
    } else {
        value
    };
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(format!("{source}: malformed {key} declaration: {value}"));
    }
    Ok(value.to_owned())
}
fn parse_declaration(source: &'static str, value: String) -> Result<Declaration, String> {
    let components = value.split('.').collect::<Vec<_>>();
    if !(2..=3).contains(&components.len())
        || components
            .iter()
            .any(|part| part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(format!(
            "{source}: unsupported Rust toolchain version declaration: {value}"
        ));
    }
    let version = (
        components[0]
            .parse()
            .map_err(|_| format!("{source}: invalid major version in {value}"))?,
        components[1]
            .parse()
            .map_err(|_| format!("{source}: invalid minor version in {value}"))?,
    );
    Ok(Declaration {
        source,
        value,
        version,
    })
}
