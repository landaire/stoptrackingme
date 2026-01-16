use serde::Deserialize;
use std::env;
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

#[derive(Debug, Deserialize)]
struct Matcher {
    #[serde(default)]
    name: String,
    hosts: Vec<String>,
    #[serde(default = "default_true")]
    terminates_matching: bool,
    #[serde(default)]
    param_matchers: Vec<Param>,
    #[serde(default)]
    path_matchers: Vec<PathComponent>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct Param {
    name: String,
    operation: ReplacementOperation,
}

#[derive(Debug, Deserialize)]
struct PathComponent {
    name: String,
    operation: ReplacementOperation,
}

#[derive(Debug, Deserialize)]
enum ReplacementOperation {
    Drop,
    ReplaceWith(String),
    RequestRedirect,
}

fn main() {
    println!("cargo:rerun-if-changed=matchers");

    let out_dir = env::var("OUT_DIR").unwrap();
    let mut matchers = Vec::new();

    for entry in WalkDir::new("matchers").into_iter().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let name = path.file_stem().unwrap().to_str().unwrap();
        let mut matcher: Matcher = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        matcher.name = name.to_string();
        if name == "global" {
            matchers.insert(0, matcher);
        } else {
            matchers.push(matcher);
        }
    }

    let matcher_count = matchers.len();

    let mut code = format!("static INCLUDED_MATCHERS: LazyLock<[Matcher; {matcher_count}]> = LazyLock::new(||[\n");
    for m in &matchers {
        code.push_str(&format!(
            "Matcher {{ name: {:?}.into(), hosts: vec![{}], terminates_matching: {}, param_matchers: vec![{}], path_matchers: vec![{}] }},\n",
            m.name,
            format_string_collection(&m.hosts),
            m.terminates_matching,
            m.param_matchers.iter().map(|p| format!("Param {{ name: {:?}.into(), operation: {} }}", p.name, op(&p.operation))).collect::<Vec<_>>().join(", "),
            m.path_matchers.iter().map(|p| format!("PathComponent {{ name: {:?}.into(), operation: {} }}", p.name, op(&p.operation))).collect::<Vec<_>>().join(", "),
        ));
    }
    code.push_str("]);");

    fs::write(Path::new(&out_dir).join("included_matchers.rs"), code).unwrap();
}

fn format_string_collection(items: &[String]) -> String {
    const INTO_TEXT: &str = ".into()";
    // Add 3 to each item -- 2 for the quote pairs and 1 for the comma
    let total_capacity = items.iter().fold(0, |accum, item| accum + 3 + item.len() + INTO_TEXT.len());
    let mut output = String::with_capacity(total_capacity);

    for item in items {
        output.push('"');
        output.push_str(item);
        output.push('"');
        output.push_str(INTO_TEXT);
        output.push(',');
    }

    output
}

fn op(o: &ReplacementOperation) -> String {
    match o {
        ReplacementOperation::Drop => "ReplacementOperation::Drop".into(),
        ReplacementOperation::ReplaceWith(s) => {
            format!("ReplacementOperation::ReplaceWith({:?}.into())", s)
        }
        ReplacementOperation::RequestRedirect => "ReplacementOperation::RequestRedirect".into(),
    }
}
