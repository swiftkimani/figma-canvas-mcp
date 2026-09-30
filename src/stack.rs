//! Work out what the target project is actually built with.
//!
//! Generated code that does not match the codebase it lands in is rework, not
//! output. A React shop on Tailwind with a `@/` alias should get exactly that,
//! not a guess at `./components/Button` and a stylesheet nobody uses.
//!
//! Everything here is inferred from files on disk, and every conclusion carries
//! the evidence that produced it. A detector that cannot say *why* it decided
//! something is impossible to trust or to correct.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::codegen::StyleMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Framework {
    #[default]
    React,
    Preact,
    Vue,
    Svelte,
    Angular,
    Solid,
    /// Nothing recognisable; React is emitted as the default.
    Unknown,
}

impl Framework {
    pub fn label(self) -> &'static str {
        match self {
            Framework::React => "React",
            Framework::Preact => "Preact",
            Framework::Vue => "Vue",
            Framework::Svelte => "Svelte",
            Framework::Angular => "Angular",
            Framework::Solid => "Solid",
            Framework::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stack {
    pub framework: Framework,
    pub typescript: bool,
    pub styling: StyleMode,
    /// e.g. ("@/", "src/") from tsconfig paths — how this project imports.
    pub alias: Option<(String, String)>,
    /// Where components live, relative to the project root.
    pub component_dir: Option<String>,
    pub single_quotes: bool,
    pub semicolons: bool,
    /// Next.js app router needs "use client" on anything interactive.
    pub react_server_components: bool,
    /// Why each conclusion was reached, so a wrong guess is correctable.
    pub evidence: Vec<String>,
    /// True when nothing was found and these are defaults, not detections.
    pub guessed: bool,
}

impl Default for Stack {
    fn default() -> Self {
        Self {
            framework: Framework::React,
            typescript: true,
            styling: StyleMode::CssModules,
            alias: None,
            component_dir: None,
            single_quotes: false,
            semicolons: true,
            react_server_components: false,
            evidence: Vec::new(),
            guessed: true,
        }
    }
}

impl Stack {
    /// A one-line summary for the caller, naming what was actually observed.
    pub fn summary(&self) -> String {
        if self.guessed {
            return "No project detected; emitting React + TypeScript + CSS modules by default. \
                    Point --project-root at your repo to match its conventions."
                .to_string();
        }
        let mut parts = vec![
            self.framework.label().to_string(),
            if self.typescript {
                "TypeScript"
            } else {
                "JavaScript"
            }
            .to_string(),
            match self.styling {
                StyleMode::CssModules => "CSS modules",
                StyleMode::Tailwind => "Tailwind",
                StyleMode::Inline => "inline styles",
            }
            .to_string(),
        ];
        if let Some((from, _)) = &self.alias {
            parts.push(format!("alias {from}"));
        }
        if self.react_server_components {
            parts.push("app router".into());
        }
        format!("Detected: {}", parts.join(" · "))
    }

    /// The import path for a component, following the project's own conventions.
    pub fn import_path(&self, component: &str) -> String {
        let dir = self.component_dir.as_deref().unwrap_or("components");
        match &self.alias {
            // `@/` maps to a source root, so strip that prefix from the path.
            Some((prefix, target)) => {
                let rel = dir
                    .strip_prefix(target.trim_end_matches('/'))
                    .unwrap_or(dir);
                let rel = rel.trim_start_matches('/');
                format!("{prefix}{rel}/{component}")
            }
            None => format!("./{}/{component}", dir.trim_start_matches("./")),
        }
    }

    pub fn file_extension(&self) -> &'static str {
        match (self.framework, self.typescript) {
            (Framework::Vue, _) => "vue",
            (Framework::Svelte, _) => "svelte",
            (_, true) => "tsx",
            (_, false) => "jsx",
        }
    }
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let raw = fs::read_to_string(path).ok()?;
    // tsconfig.json routinely contains comments, which serde_json rejects.
    let cleaned = strip_json_comments(&raw);
    serde_json::from_str(&cleaned).ok()
}

/// Strip `//` and `/* */` comments, and trailing commas, outside of strings.
fn strip_json_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
            }
            _ => out.push(c),
        }
    }

    // Trailing commas before } or ], which tsconfig also tolerates.
    let mut cleaned = String::with_capacity(out.len());
    let bytes: Vec<char> = out.chars().collect();
    for (i, &c) in bytes.iter().enumerate() {
        if c == ','
            && let Some(next) = bytes[i + 1..].iter().find(|c| !c.is_whitespace())
            && (*next == '}' || *next == ']')
        {
            continue;
        }
        cleaned.push(c);
    }
    cleaned
}

/// Merge `dependencies` and `devDependencies` into one lookup.
fn all_deps(pkg: &serde_json::Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for key in ["dependencies", "devDependencies", "peerDependencies"] {
        if let Some(map) = pkg.get(key).and_then(|v| v.as_object()) {
            for (k, v) in map {
                out.insert(k.clone(), v.as_str().unwrap_or("").to_string());
            }
        }
    }
    out
}

/// Does any file matching `suffix` exist within `depth` levels of `root`?
fn has_file_with_suffix(root: &Path, suffix: &str, depth: usize) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let entries = fs::read_dir(root).ok()?;
    let mut dirs = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        // Never walk into dependency or build output trees.
        if name.starts_with('.') || name == "node_modules" || name == "target" || name == "dist" {
            continue;
        }
        if path.is_dir() {
            dirs.push(path);
        } else if name.ends_with(suffix) {
            return Some(path);
        }
    }
    for d in dirs {
        if let Some(found) = has_file_with_suffix(&d, suffix, depth - 1) {
            return Some(found);
        }
    }
    None
}

/// Inspect a project directory and report what it is built with.
pub fn detect(root: &Path) -> Stack {
    let mut s = Stack::default();
    let pkg_path = root.join("package.json");
    let Some(pkg) = read_json(&pkg_path) else {
        return s;
    };
    s.guessed = false;
    s.evidence.push("package.json found".into());

    let deps = all_deps(&pkg);
    let has = |name: &str| deps.contains_key(name);

    // --- framework ---
    // Order matters: Next implies React, Nuxt implies Vue.
    s.framework = if has("@angular/core") {
        s.evidence.push("@angular/core in dependencies".into());
        Framework::Angular
    } else if has("svelte") {
        s.evidence.push("svelte in dependencies".into());
        Framework::Svelte
    } else if has("vue") || has("nuxt") {
        s.evidence.push(format!(
            "{} in dependencies",
            if has("nuxt") { "nuxt" } else { "vue" }
        ));
        Framework::Vue
    } else if has("solid-js") {
        s.evidence.push("solid-js in dependencies".into());
        Framework::Solid
    } else if has("preact") && !has("react") {
        s.evidence.push("preact in dependencies".into());
        Framework::Preact
    } else if has("react") || has("next") {
        s.evidence.push(format!(
            "{} in dependencies",
            if has("next") { "next" } else { "react" }
        ));
        Framework::React
    } else {
        s.evidence
            .push("no framework dependency recognised; defaulting to React".into());
        Framework::Unknown
    };

    // --- language ---
    let tsconfig = root.join("tsconfig.json");
    s.typescript = has("typescript") || tsconfig.exists();
    s.evidence.push(if s.typescript {
        "TypeScript: tsconfig.json or typescript dependency".into()
    } else {
        "no TypeScript signal; emitting JavaScript".to_string()
    });

    // --- styling ---
    if has("tailwindcss") {
        s.styling = StyleMode::Tailwind;
        s.evidence.push("tailwindcss in dependencies".into());
    } else if has("styled-components") || has("@emotion/react") || has("@emotion/styled") {
        // We do not emit CSS-in-JS, so say so rather than silently mismatching.
        s.styling = StyleMode::Inline;
        s.evidence.push(
            "CSS-in-JS detected (styled-components/emotion); emitting inline styles, \
             which are the closest thing this tool generates"
                .into(),
        );
    } else if let Some(found) = has_file_with_suffix(root, ".module.css", 4) {
        s.styling = StyleMode::CssModules;
        s.evidence
            .push(format!("CSS modules in use, e.g. {}", found.display()));
    } else {
        s.evidence
            .push("no styling signal; defaulting to CSS modules".into());
    }

    // --- Next.js app router needs "use client" for interactivity ---
    if has("next") && (root.join("app").is_dir() || root.join("src/app").is_dir()) {
        s.react_server_components = true;
        s.evidence
            .push("Next.js app/ directory: client components need \"use client\"".into());
    }

    // --- import alias ---
    if let Some(tsc) = read_json(&tsconfig)
        && let Some(paths) = tsc
            .pointer("/compilerOptions/paths")
            .and_then(|v| v.as_object())
    {
        for (k, v) in paths {
            let target = v
                .as_array()
                .and_then(|a| a.first())
                .and_then(|t| t.as_str())
                .unwrap_or("");
            if let (Some(prefix), Some(t)) = (k.strip_suffix('*'), target.strip_suffix('*')) {
                let t = t.trim_start_matches("./");
                s.alias = Some((prefix.to_string(), t.to_string()));
                s.evidence
                    .push(format!("tsconfig path alias {k} -> {target}"));
                break;
            }
        }
    }

    // --- where components live ---
    for candidate in [
        "src/components",
        "app/components",
        "components",
        "src/ui",
        "src/lib/components",
    ] {
        if root.join(candidate).is_dir() {
            s.component_dir = Some(candidate.to_string());
            s.evidence
                .push(format!("components directory: {candidate}"));
            break;
        }
    }

    // --- formatting, so output does not fight the linter ---
    let prettier = read_json(&root.join(".prettierrc"))
        .or_else(|| read_json(&root.join(".prettierrc.json")))
        .or_else(|| pkg.get("prettier").cloned());
    if let Some(p) = prettier {
        if let Some(q) = p.get("singleQuote").and_then(|v| v.as_bool()) {
            s.single_quotes = q;
        }
        if let Some(semi) = p.get("semi").and_then(|v| v.as_bool()) {
            s.semicolons = semi;
        }
        s.evidence.push(format!(
            "prettier config: {} quotes, semicolons {}",
            if s.single_quotes { "single" } else { "double" },
            if s.semicolons { "on" } else { "off" }
        ));
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fcm-stack-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    #[test]
    fn no_project_is_admitted_as_a_guess_not_a_detection() {
        let dir = tmp("empty");
        let s = detect(&dir);
        assert!(s.guessed);
        assert!(s.summary().contains("No project detected"));
        // The defaults are still usable.
        assert_eq!(s.framework, Framework::React);
        assert_eq!(s.import_path("Button"), "./components/Button");
    }

    #[test]
    fn a_next_tailwind_project_is_read_correctly() {
        let dir = tmp("next");
        write(
            &dir,
            "package.json",
            r#"{"dependencies":{"next":"15.0.0","react":"19.0.0"},
                "devDependencies":{"typescript":"5.6.0","tailwindcss":"3.4.0"}}"#,
        );
        write(
            &dir,
            "tsconfig.json",
            r#"{
                // Next.js default
                "compilerOptions": { "paths": { "@/*": ["./src/*"] } },
            }"#,
        );
        fs::create_dir_all(dir.join("src/app")).unwrap();
        fs::create_dir_all(dir.join("src/components")).unwrap();

        let s = detect(&dir);
        assert!(!s.guessed);
        assert_eq!(s.framework, Framework::React);
        assert!(s.typescript);
        assert_eq!(s.styling, StyleMode::Tailwind);
        assert!(s.react_server_components, "app/ router should be detected");
        assert_eq!(s.alias, Some(("@/".into(), "src/".into())));
        // The alias absorbs the src/ prefix rather than doubling it.
        assert_eq!(s.import_path("Button"), "@/components/Button");
        assert_eq!(s.file_extension(), "tsx");
    }

    #[test]
    fn tsconfig_comments_and_trailing_commas_do_not_break_detection() {
        let dir = tmp("comments");
        write(&dir, "package.json", r#"{"dependencies":{"react":"18"}}"#);
        write(
            &dir,
            "tsconfig.json",
            r#"{
              /* block comment
                 spanning lines */
              "compilerOptions": {
                // line comment with a "quoted brace } inside
                "paths": { "~/*": ["./app/*"] },
              },
            }"#,
        );
        let s = detect(&dir);
        assert_eq!(
            s.alias,
            Some(("~/".into(), "app/".into())),
            "tsconfig comments must not defeat parsing"
        );
    }

    #[test]
    fn vue_and_svelte_are_distinguished_from_react() {
        let dir = tmp("vue");
        write(&dir, "package.json", r#"{"dependencies":{"vue":"3.4.0"}}"#);
        assert_eq!(detect(&dir).framework, Framework::Vue);
        assert_eq!(detect(&dir).file_extension(), "vue");

        let dir = tmp("svelte");
        write(
            &dir,
            "package.json",
            r#"{"devDependencies":{"svelte":"5.0.0"}}"#,
        );
        assert_eq!(detect(&dir).framework, Framework::Svelte);
    }

    #[test]
    fn css_modules_are_detected_from_actual_files() {
        let dir = tmp("cssmod");
        write(&dir, "package.json", r#"{"dependencies":{"react":"18"}}"#);
        write(&dir, "src/ui/Card.module.css", ".card { color: red; }");
        let s = detect(&dir);
        assert_eq!(s.styling, StyleMode::CssModules);
        assert!(
            s.evidence.iter().any(|e| e.contains("Card.module.css")),
            "evidence must name the file that proved it: {:?}",
            s.evidence
        );
    }

    #[test]
    fn node_modules_is_never_walked() {
        let dir = tmp("nomodules");
        write(&dir, "package.json", r#"{"dependencies":{"react":"18"}}"#);
        // A dependency's own CSS modules must not be mistaken for the project's.
        write(&dir, "node_modules/x/y.module.css", ".a{}");
        let s = detect(&dir);
        assert!(
            !s.evidence.iter().any(|e| e.contains("node_modules")),
            "node_modules must not be inspected: {:?}",
            s.evidence
        );
    }

    #[test]
    fn prettier_preferences_are_picked_up() {
        let dir = tmp("prettier");
        write(
            &dir,
            "package.json",
            r#"{"dependencies":{"react":"18"},"prettier":{"singleQuote":true,"semi":false}}"#,
        );
        let s = detect(&dir);
        assert!(s.single_quotes);
        assert!(!s.semicolons);
    }
}
