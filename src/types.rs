use serde::{Deserialize, Serialize};

// ── Parsing types ────────────────────────────────────────────

/// A symbol extracted from a source file by tree-sitter.
#[derive(Debug, Clone)]
pub struct ParsedSymbol {
    pub name: String,
    pub symbol_type: String, // "function_declaration", "class_declaration", etc.
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub code: String,
}

/// Results from parsing a single file.
#[derive(Debug, Default)]
pub struct FileParseResult {
    pub file_path: String,
    pub symbols: Vec<ParsedSymbol>,
    pub calls: Vec<String>,
    pub imports: Vec<String>,
}

// ── MCP response types ──────────────────────────────────────

/// A search hit from FTS5.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub file: String,
    pub name: String,
    #[serde(rename = "type")]
    pub symbol_type: String,
    pub start_line: u32,
    pub end_line: u32,
    pub text: String,
    pub score: f64,
}

/// A symbol lookup result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolResult {
    pub file: String,
    pub name: String,
    #[serde(rename = "type")]
    pub symbol_type: String,
    pub start_line: u32,
    pub end_line: u32,
    pub text: String,
}

/// Call graph context for a symbol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphContext {
    pub symbol: String,
    pub defined_in: Vec<String>,
    pub called_by: Vec<String>,
}

/// A chunk from analyze_bug with both search score and graph context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisChunk {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub code_snippet: String,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph_context: Option<GraphContext>,
}

/// Top-level result from analyze_bug.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisResult {
    pub query: String,
    pub analysis_context: Vec<AnalysisChunk>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsed_symbol_clone_and_debug() {
        let sym = ParsedSymbol {
            name: "foo".into(),
            symbol_type: "function_declaration".into(),
            file_path: "src/lib.rs".into(),
            start_line: 1,
            end_line: 5,
            code: "fn foo() {}".into(),
        };
        let cloned = sym.clone();
        assert_eq!(cloned.name, "foo");
        assert_eq!(cloned.symbol_type, "function_declaration");
        assert_eq!(cloned.file_path, "src/lib.rs");
        assert_eq!(cloned.start_line, 1);
        assert_eq!(cloned.end_line, 5);
        assert_eq!(cloned.code, "fn foo() {}");
        // Debug trait works
        let debug_str = format!("{:?}", cloned);
        assert!(debug_str.contains("foo"));
    }

    #[test]
    fn file_parse_result_default() {
        let result = FileParseResult::default();
        assert!(result.file_path.is_empty());
        assert!(result.symbols.is_empty());
        assert!(result.calls.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn search_result_serialize_deserialize() {
        let sr = SearchResult {
            file: "test.js".into(),
            name: "myFunc".into(),
            symbol_type: "function_declaration".into(),
            start_line: 10,
            end_line: 20,
            text: "function myFunc() {}".into(),
            score: 0.95,
        };
        let json = serde_json::to_string(&sr).unwrap();
        assert!(json.contains("\"type\":\"function_declaration\""));
        assert!(!json.contains("symbol_type"));

        let deserialized: SearchResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.file, "test.js");
        assert_eq!(deserialized.name, "myFunc");
        assert_eq!(deserialized.symbol_type, "function_declaration");
        assert_eq!(deserialized.start_line, 10);
        assert_eq!(deserialized.end_line, 20);
        assert_eq!(deserialized.score, 0.95);
    }

    #[test]
    fn symbol_result_serialize_deserialize() {
        let sr = SymbolResult {
            file: "app.ts".into(),
            name: "MyClass".into(),
            symbol_type: "class_declaration".into(),
            start_line: 1,
            end_line: 50,
            text: "class MyClass {}".into(),
        };
        let json = serde_json::to_string(&sr).unwrap();
        assert!(json.contains("\"type\":\"class_declaration\""));

        let deserialized: SymbolResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "MyClass");
        assert_eq!(deserialized.symbol_type, "class_declaration");
    }

    #[test]
    fn graph_context_serialize_deserialize() {
        let gc = GraphContext {
            symbol: "handleClick".into(),
            defined_in: vec!["src/handler.js".into()],
            called_by: vec!["src/app.js".into(), "src/main.js".into()],
        };
        let json = serde_json::to_string(&gc).unwrap();
        let deserialized: GraphContext = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.symbol, "handleClick");
        assert_eq!(deserialized.defined_in.len(), 1);
        assert_eq!(deserialized.called_by.len(), 2);
    }

    #[test]
    fn analysis_chunk_with_optional_fields() {
        // With all optional fields present
        let chunk = AnalysisChunk {
            file: "src/utils.js".into(),
            symbol: Some("calculate".into()),
            code_snippet: "function calculate() {}".into(),
            score: 0.85,
            graph_context: Some(GraphContext {
                symbol: "calculate".into(),
                defined_in: vec!["src/utils.js".into()],
                called_by: vec![],
            }),
        };
        let json = serde_json::to_string(&chunk).unwrap();
        assert!(json.contains("calculate"));
        assert!(json.contains("graph_context"));

        // With optional fields absent
        let chunk_minimal = AnalysisChunk {
            file: "src/utils.js".into(),
            symbol: None,
            code_snippet: "some code".into(),
            score: 0.5,
            graph_context: None,
        };
        let json_minimal = serde_json::to_string(&chunk_minimal).unwrap();
        assert!(!json_minimal.contains("symbol"));
        assert!(!json_minimal.contains("graph_context"));
    }

    #[test]
    fn analysis_result_serialize_deserialize() {
        let result = AnalysisResult {
            query: "memory leak".into(),
            analysis_context: vec![AnalysisChunk {
                file: "src/cache.js".into(),
                symbol: Some("clearCache".into()),
                code_snippet: "function clearCache() {}".into(),
                score: 0.9,
                graph_context: None,
            }],
        };
        let json = serde_json::to_string_pretty(&result).unwrap();
        let deserialized: AnalysisResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.query, "memory leak");
        assert_eq!(deserialized.analysis_context.len(), 1);
        assert_eq!(deserialized.analysis_context[0].file, "src/cache.js");
    }
}
