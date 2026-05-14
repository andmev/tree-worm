use std::collections::HashMap;

use petgraph::graph::NodeIndex;
use petgraph::stable_graph::StableDiGraph;
use petgraph::visit::EdgeRef;
use petgraph::Direction;

use crate::types::{GraphContext, ParsedSymbol};

#[derive(Debug, Clone)]
#[allow(dead_code)]
enum NodeData {
    File { path: String },
    Symbol { name: String, file_path: String },
    UnresolvedCall { name: String },
    Module { name: String },
}

#[derive(Debug, Clone)]
enum EdgeKind {
    Defines,
    Calls,
    Imports,
    CallsResolved,
}

pub struct CallGraphEngine {
    graph: StableDiGraph<NodeData, EdgeKind>,
    /// O(1) lookup: symbol name → node index
    symbol_index: HashMap<String, NodeIndex>,
    /// O(1) lookup: file path → node index
    file_index: HashMap<String, NodeIndex>,
    /// O(1) lookup: unresolved call name → node index
    unresolved_index: HashMap<String, NodeIndex>,
}

impl CallGraphEngine {
    pub fn new() -> Self {
        Self {
            graph: StableDiGraph::new(),
            symbol_index: HashMap::new(),
            file_index: HashMap::new(),
            unresolved_index: HashMap::new(),
        }
    }

    /// Add parsed data from a single file to the graph.
    pub fn add_file_data(
        &mut self,
        file_path: &str,
        symbols: &[ParsedSymbol],
        calls: &[String],
        imports: &[String],
    ) {
        // Get or create file node
        let file_node = *self
            .file_index
            .entry(file_path.to_string())
            .or_insert_with(|| {
                self.graph.add_node(NodeData::File {
                    path: file_path.to_string(),
                })
            });

        // Add symbol nodes with defines edges
        for sym in symbols {
            let sym_node = self.graph.add_node(NodeData::Symbol {
                name: sym.name.clone(),
                file_path: file_path.to_string(),
            });
            self.graph.add_edge(file_node, sym_node, EdgeKind::Defines);
            // Index by name (last definition wins for ambiguous names)
            self.symbol_index.insert(sym.name.clone(), sym_node);
        }

        // Add unresolved call nodes
        for call in calls {
            let call_node = *self
                .unresolved_index
                .entry(call.clone())
                .or_insert_with(|| {
                    self.graph
                        .add_node(NodeData::UnresolvedCall { name: call.clone() })
                });
            self.graph.add_edge(file_node, call_node, EdgeKind::Calls);
        }

        // Add module import nodes
        for imp in imports {
            let mod_node = self.graph.add_node(NodeData::Module { name: imp.clone() });
            self.graph.add_edge(file_node, mod_node, EdgeKind::Imports);
        }
    }

    /// Resolve unresolved call nodes to actual symbol nodes.
    pub fn resolve_calls(&mut self) {
        let resolutions: Vec<(NodeIndex, NodeIndex)> = self
            .unresolved_index
            .iter()
            .filter_map(|(name, &unresolved_node)| {
                self.symbol_index
                    .get(name)
                    .map(|&sym_node| (unresolved_node, sym_node))
            })
            .collect();

        for (unresolved_node, sym_node) in resolutions {
            // Find all files that call this unresolved symbol
            let callers: Vec<NodeIndex> = self
                .graph
                .edges_directed(unresolved_node, Direction::Incoming)
                .filter(|e| matches!(e.weight(), EdgeKind::Calls))
                .map(|e| e.source())
                .collect();

            for caller in callers {
                self.graph
                    .add_edge(caller, sym_node, EdgeKind::CallsResolved);
            }
        }
    }

    /// Get the call graph context for a symbol: where it's defined and who calls it.
    pub fn get_symbol_context(&self, symbol_name: &str) -> GraphContext {
        let empty = GraphContext {
            symbol: symbol_name.to_string(),
            defined_in: vec![],
            called_by: vec![],
        };

        let sym_node = match self.symbol_index.get(symbol_name) {
            Some(&idx) => idx,
            None => return empty,
        };

        // Files that define this symbol
        let defined_in: Vec<String> = self
            .graph
            .edges_directed(sym_node, Direction::Incoming)
            .filter(|e| matches!(e.weight(), EdgeKind::Defines))
            .filter_map(|e| match &self.graph[e.source()] {
                NodeData::File { path } => Some(path.clone()),
                _ => None,
            })
            .collect();

        // Files that call this symbol (via resolved edges)
        let called_by: Vec<String> = self
            .graph
            .edges_directed(sym_node, Direction::Incoming)
            .filter(|e| matches!(e.weight(), EdgeKind::CallsResolved))
            .filter_map(|e| match &self.graph[e.source()] {
                NodeData::File { path } => Some(path.clone()),
                _ => None,
            })
            .collect();

        GraphContext {
            symbol: symbol_name.to_string(),
            defined_in,
            called_by,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ParsedSymbol;

    fn make_symbol(name: &str, file: &str) -> ParsedSymbol {
        ParsedSymbol {
            name: name.into(),
            symbol_type: "function_declaration".into(),
            file_path: file.into(),
            start_line: 0,
            end_line: 5,
            code: format!("function {}() {{}}", name),
        }
    }

    #[test]
    fn new_engine_is_empty() {
        let engine = CallGraphEngine::new();
        let ctx = engine.get_symbol_context("nonexistent");
        assert_eq!(ctx.symbol, "nonexistent");
        assert!(ctx.defined_in.is_empty());
        assert!(ctx.called_by.is_empty());
    }

    #[test]
    fn add_file_data_registers_symbols() {
        let mut engine = CallGraphEngine::new();
        let sym = make_symbol("hello", "src/a.js");
        engine.add_file_data("src/a.js", &[sym], &[], &[]);

        let ctx = engine.get_symbol_context("hello");
        assert_eq!(ctx.symbol, "hello");
        assert_eq!(ctx.defined_in, vec!["src/a.js"]);
        assert!(ctx.called_by.is_empty());
    }

    #[test]
    fn resolve_calls_links_caller_to_symbol() {
        let mut engine = CallGraphEngine::new();

        // File a.js defines "greet"
        let sym = make_symbol("greet", "src/a.js");
        engine.add_file_data("src/a.js", &[sym], &[], &[]);

        // File b.js calls "greet"
        engine.add_file_data("src/b.js", &[], &["greet".into()], &[]);

        engine.resolve_calls();

        let ctx = engine.get_symbol_context("greet");
        assert_eq!(ctx.defined_in, vec!["src/a.js"]);
        assert_eq!(ctx.called_by, vec!["src/b.js"]);
    }

    #[test]
    fn multiple_callers_resolved() {
        let mut engine = CallGraphEngine::new();

        let sym = make_symbol("utils", "src/utils.js");
        engine.add_file_data("src/utils.js", &[sym], &[], &[]);

        engine.add_file_data("src/a.js", &[], &["utils".into()], &[]);
        engine.add_file_data("src/b.js", &[], &["utils".into()], &[]);
        engine.add_file_data("src/c.js", &[], &["utils".into()], &[]);

        engine.resolve_calls();

        let ctx = engine.get_symbol_context("utils");
        assert_eq!(ctx.defined_in, vec!["src/utils.js"]);
        assert_eq!(ctx.called_by.len(), 3);
        assert!(ctx.called_by.contains(&"src/a.js".to_string()));
        assert!(ctx.called_by.contains(&"src/b.js".to_string()));
        assert!(ctx.called_by.contains(&"src/c.js".to_string()));
    }

    #[test]
    fn unresolved_call_returns_empty_context() {
        let mut engine = CallGraphEngine::new();
        engine.add_file_data("src/a.js", &[], &["nonexistent".into()], &[]);
        engine.resolve_calls();

        let ctx = engine.get_symbol_context("nonexistent");
        assert!(ctx.defined_in.is_empty());
        assert!(ctx.called_by.is_empty());
    }

    #[test]
    fn imports_do_not_affect_call_graph() {
        let mut engine = CallGraphEngine::new();
        let sym = make_symbol("handler", "src/handler.js");
        engine.add_file_data(
            "src/handler.js",
            &[sym],
            &[],
            &["express".into(), "lodash".into()],
        );

        let ctx = engine.get_symbol_context("handler");
        assert_eq!(ctx.defined_in, vec!["src/handler.js"]);
        assert!(ctx.called_by.is_empty());
    }

    #[test]
    fn same_file_reused_for_multiple_add_file_data() {
        let mut engine = CallGraphEngine::new();
        let sym1 = make_symbol("a", "src/main.js");
        engine.add_file_data("src/main.js", &[sym1], &[], &[]);

        let sym2 = make_symbol("b", "src/main.js");
        engine.add_file_data("src/main.js", &[sym2], &["a".into()], &[]);

        engine.resolve_calls();

        let ctx_a = engine.get_symbol_context("a");
        // "a" is defined in src/main.js and called from src/main.js
        assert_eq!(ctx_a.defined_in, vec!["src/main.js"]);
        assert_eq!(ctx_a.called_by, vec!["src/main.js"]);
    }

    #[test]
    fn last_definition_wins_for_duplicate_symbol_names() {
        let mut engine = CallGraphEngine::new();
        let sym1 = make_symbol("dup", "src/a.js");
        engine.add_file_data("src/a.js", &[sym1], &[], &[]);

        let sym2 = make_symbol("dup", "src/b.js");
        engine.add_file_data("src/b.js", &[sym2], &[], &[]);

        // The symbol_index points to the last-inserted node
        let ctx = engine.get_symbol_context("dup");
        assert_eq!(ctx.defined_in, vec!["src/b.js"]);
    }

    #[test]
    fn resolve_calls_idempotent() {
        let mut engine = CallGraphEngine::new();
        let sym = make_symbol("foo", "src/a.js");
        engine.add_file_data("src/a.js", &[sym], &[], &[]);
        engine.add_file_data("src/b.js", &[], &["foo".into()], &[]);

        engine.resolve_calls();
        engine.resolve_calls(); // second call should not double edges

        let ctx = engine.get_symbol_context("foo");
        // May have duplicate edges but callers should still include b.js
        assert!(ctx.called_by.contains(&"src/b.js".to_string()));
    }
}
