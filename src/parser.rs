use anyhow::{Context, Result};
use tree_sitter::{Node, Parser, TreeCursor};

use crate::types::{FileParseResult, ParsedSymbol};

/// Select the tree-sitter language grammar based on file extension.
fn get_language(file_path: &str) -> Option<tree_sitter::Language> {
    if file_path.ends_with(".js") || file_path.ends_with(".jsx") {
        Some(tree_sitter_javascript::LANGUAGE.into())
    } else if file_path.ends_with(".tsx") {
        Some(tree_sitter_typescript::LANGUAGE_TSX.into())
    } else if file_path.ends_with(".ts") {
        Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
    } else {
        None
    }
}

/// Parse a source file and extract symbols, calls, and imports.
pub fn parse_file(file_path: &str, code: &str) -> Result<FileParseResult> {
    let language = match get_language(file_path) {
        Some(lang) => lang,
        None => {
            return Ok(FileParseResult {
                file_path: file_path.to_string(),
                ..Default::default()
            });
        }
    };

    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .context("Failed to set tree-sitter language")?;

    let tree = parser
        .parse(code, None)
        .context("Failed to parse source file")?;

    let source = code.as_bytes();
    let mut result = FileParseResult {
        file_path: file_path.to_string(),
        ..Default::default()
    };

    let mut cursor = tree.root_node().walk();
    traverse(
        tree.root_node(),
        source,
        &mut result,
        file_path,
        &mut cursor,
    );

    Ok(result)
}

/// Recursively traverse the AST, extracting symbols, calls, and imports.
fn traverse<'a>(
    node: Node<'a>,
    source: &[u8],
    result: &mut FileParseResult,
    file_path: &str,
    cursor: &mut TreeCursor<'a>,
) {
    let kind = node.kind();

    // --- Imports ---
    if kind == "import_statement" || kind == "import_declaration" {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                if child.kind() == "string" {
                    if let Ok(text) = child.utf8_text(source) {
                        let module = text.trim_matches('\"').trim_matches('\'');
                        result.imports.push(module.to_string());
                    }
                }
            }
        }
    }

    // --- Symbol definitions ---
    let mut symbol_name: Option<String> = None;

    match kind {
        "function_declaration" | "method_definition" | "function_expression" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source) {
                    symbol_name = Some(name.to_string());
                }
            }
        }
        "arrow_function" => {
            // Arrow functions: check parent for variable_declarator
            // e.g. const myFunc = () => {}
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source) {
                    symbol_name = Some(name.to_string());
                }
            } else if let Some(parent) = node.parent() {
                if parent.kind() == "variable_declarator" {
                    if let Some(name_node) = parent.child_by_field_name("name") {
                        if let Ok(name) = name_node.utf8_text(source) {
                            symbol_name = Some(name.to_string());
                        }
                    }
                }
            }
        }
        "class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source) {
                    symbol_name = Some(name.to_string());
                }
            }
        }
        _ => {}
    }

    if let Some(name) = symbol_name {
        if let Ok(code_text) = node.utf8_text(source) {
            result.symbols.push(ParsedSymbol {
                name,
                symbol_type: kind.to_string(),
                file_path: file_path.to_string(),
                start_line: node.start_position().row,
                end_line: node.end_position().row,
                code: code_text.to_string(),
            });
        }
    }

    // --- Call expressions ---
    if kind == "call_expression" {
        if let Some(func_node) = node.child_by_field_name("function") {
            if let Ok(text) = func_node.utf8_text(source) {
                result.calls.push(text.to_string());
            }
        }
    }

    // --- Recurse into children ---
    for child in node.children(cursor) {
        let mut child_cursor = child.walk();
        traverse(child, source, result, file_path, &mut child_cursor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_unsupported_file_returns_empty() {
        let result = parse_file("main.rs", "fn main() {}").unwrap();
        assert_eq!(result.file_path, "main.rs");
        assert!(result.symbols.is_empty());
        assert!(result.calls.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn parse_js_function_declaration() {
        let code = "function greet(name) { return 'Hello ' + name; }";
        let result = parse_file("app.js", code).unwrap();
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert_eq!(result.symbols[0].symbol_type, "function_declaration");
        assert_eq!(result.symbols[0].file_path, "app.js");
        assert_eq!(result.symbols[0].start_line, 0);
    }

    #[test]
    fn parse_ts_function() {
        let code = "function add(a: number, b: number): number { return a + b; }";
        let result = parse_file("math.ts", code).unwrap();
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "add");
    }

    #[test]
    fn parse_jsx_file() {
        let code = r#"function App() { return <div>Hello</div>; }"#;
        let result = parse_file("App.jsx", code).unwrap();
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "App");
    }

    #[test]
    fn parse_tsx_file() {
        let code = r#"function App(): JSX.Element { return <div>Hello</div>; }"#;
        let result = parse_file("App.tsx", code).unwrap();
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "App");
    }

    #[test]
    fn parse_arrow_function_in_variable() {
        let code = "const greet = (name) => { return 'Hello ' + name; };";
        let result = parse_file("app.js", code).unwrap();
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert_eq!(result.symbols[0].symbol_type, "arrow_function");
    }

    #[test]
    fn parse_class_declaration() {
        let code = "class UserService { constructor() {} getUser() { return null; } }";
        let result = parse_file("service.js", code).unwrap();
        // class + method_definition(s)
        let class_syms: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.symbol_type == "class_declaration")
            .collect();
        assert_eq!(class_syms.len(), 1);
        assert_eq!(class_syms[0].name, "UserService");

        let methods: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.symbol_type == "method_definition")
            .collect();
        assert!(methods.len() >= 1); // at least getUser
    }

    #[test]
    fn parse_call_expressions() {
        let code = "function main() { console.log('hello'); fetch('/api'); }";
        let result = parse_file("app.js", code).unwrap();
        assert!(result.calls.contains(&"console.log".to_string()));
        assert!(result.calls.contains(&"fetch".to_string()));
    }

    #[test]
    fn parse_import_statements() {
        let code = r#"import express from 'express';
import { Router } from 'express';
function app() {}"#;
        let result = parse_file("server.js", code).unwrap();
        assert!(result.imports.contains(&"express".to_string()));
    }

    #[test]
    fn parse_ts_import() {
        let code = r#"import { join } from 'path';
export function resolve() { return join('a', 'b'); }"#;
        let result = parse_file("utils.ts", code).unwrap();
        assert!(!result.imports.is_empty());
        assert!(result.imports.iter().any(|i| i == "path"));
    }

    #[test]
    fn parse_multiple_functions() {
        let code = r#"
function a() { return 1; }
function b() { return 2; }
function c() { return a() + b(); }
"#;
        let result = parse_file("multi.js", code).unwrap();
        assert_eq!(result.symbols.len(), 3);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
        assert!(names.contains(&"c"));
        // c calls a and b
        assert!(result.calls.contains(&"a".to_string()));
        assert!(result.calls.contains(&"b".to_string()));
    }

    #[test]
    fn parse_empty_file() {
        let result = parse_file("empty.js", "").unwrap();
        assert!(result.symbols.is_empty());
        assert!(result.calls.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn parse_file_path_preserved() {
        let code = "function x() {}";
        let result = parse_file("src/deep/nested/file.js", code).unwrap();
        assert_eq!(result.file_path, "src/deep/nested/file.js");
        assert_eq!(result.symbols[0].file_path, "src/deep/nested/file.js");
    }

    #[test]
    fn parse_symbol_line_numbers() {
        let code = "\n\nfunction foo() {\n  return 42;\n}";
        let result = parse_file("lines.js", code).unwrap();
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].start_line, 2);
        assert_eq!(result.symbols[0].end_line, 4);
    }

    #[test]
    fn parse_symbol_code_captured() {
        let code = "function hello() { return 'world'; }";
        let result = parse_file("code.js", code).unwrap();
        assert_eq!(result.symbols[0].code, code);
    }

    #[test]
    fn get_language_returns_none_for_unsupported() {
        assert!(get_language("file.py").is_none());
        assert!(get_language("file.rs").is_none());
        assert!(get_language("file.go").is_none());
        assert!(get_language("file").is_none());
    }

    #[test]
    fn get_language_returns_some_for_supported() {
        assert!(get_language("file.js").is_some());
        assert!(get_language("file.jsx").is_some());
        assert!(get_language("file.ts").is_some());
        assert!(get_language("file.tsx").is_some());
    }
}
