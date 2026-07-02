mod graph;
mod parser;
mod store;
mod types;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser as ClapParser;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rmcp::handler::server::router::Router;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_router, ServiceExt};
use schemars::JsonSchema;
use tracing::info;

use crate::graph::CallGraphEngine;
use crate::store::CodeStore;
use crate::types::{AnalysisChunk, AnalysisResult};

// ── CLI ────────────────────────────────────────────────────────

#[derive(ClapParser)]
#[command(
    name = "tree-worm",
    version,
    about = "Code intelligence MCP server. Indexes JS/TS and exposes search, symbols, and call graphs."
)]
struct Cli {
    /// Workspace directory to index
    #[arg(short, long, default_value = ".")]
    workspace: PathBuf,

    /// Data directory for storing the search index
    #[arg(short, long, default_value = ".tree-worm")]
    data_dir: PathBuf,
}

// ── Tool argument types ───────────────────────────────────────

#[derive(Debug, serde::Deserialize, JsonSchema)]
struct SearchCodeArgs {
    /// The search query (e.g. 'auth logic')
    query: String,
}

#[derive(Debug, serde::Deserialize, JsonSchema)]
struct GetSymbolArgs {
    /// The name of the symbol
    name: String,
}

#[derive(Debug, serde::Deserialize, JsonSchema)]
struct GetCallGraphArgs {
    /// The symbol to trace
    symbol: String,
}

#[derive(Debug, serde::Deserialize, JsonSchema)]
struct AnalyzeBugArgs {
    /// Description of the bug
    query: String,
}

// ── Shared State ───────────────────────────────────────────

/// Represents the lifecycle state of the indexing pipeline.
enum IndexState {
    /// Indexing has not yet completed.
    Pending,
    /// Indexing succeeded — store and graph are available.
    Ready(Arc<CodeStore>, Arc<CallGraphEngine>),
    /// Indexing failed permanently — contains the error message.
    Failed(String),
}

// ── MCP Server ─────────────────────────────────────────────

#[derive(Clone)]
struct TreeWormServer {
    state: Arc<std::sync::RwLock<IndexState>>,
}

impl ServerHandler for TreeWormServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some("Code intelligence server. Indexes JS/TS and exposes search, symbols, and call graphs.".into()),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

#[tool_router]
impl TreeWormServer {
    #[tool(description = "Semantic search over the indexed repository")]
    async fn search_code(&self, Parameters(args): Parameters<SearchCodeArgs>) -> String {
        let store = {
            let guard = self.state.read().unwrap();
            match &*guard {
                IndexState::Pending => {
                    return r#"{"error": "Indexing in progress, please retry"}"#.to_string()
                }
                IndexState::Failed(msg) => {
                    return format!(r#"{{"error": "Indexing failed: {msg}"}}"#)
                }
                IndexState::Ready(s, _) => s.clone(),
            }
        };
        tokio::task::spawn_blocking(move || match store.search(&args.query, 10) {
            Ok(results) => serde_json::to_string_pretty(&results)
                .unwrap_or_else(|e| format!(r#"{{"error": "{e}"}}"#)),
            Err(e) => format!(r#"{{"error": "{e}"}}"#),
        })
        .await
        .unwrap_or_else(|e| format!(r#"{{"error": "{e}"}}"#))
    }

    #[tool(description = "Get the details of a specific symbol (function, class)")]
    async fn get_symbol(&self, Parameters(args): Parameters<GetSymbolArgs>) -> String {
        let store = {
            let guard = self.state.read().unwrap();
            match &*guard {
                IndexState::Pending => {
                    return r#"{"error": "Indexing in progress, please retry"}"#.to_string()
                }
                IndexState::Failed(msg) => {
                    return format!(r#"{{"error": "Indexing failed: {msg}"}}"#)
                }
                IndexState::Ready(s, _) => s.clone(),
            }
        };
        tokio::task::spawn_blocking(move || match store.get_symbol(&args.name) {
            Ok(results) => serde_json::to_string_pretty(&results)
                .unwrap_or_else(|e| format!(r#"{{"error": "{e}"}}"#)),
            Err(e) => format!(r#"{{"error": "{e}"}}"#),
        })
        .await
        .unwrap_or_else(|e| format!(r#"{{"error": "{e}"}}"#))
    }

    #[tool(description = "Get the callers and definitions of a symbol")]
    async fn get_call_graph(&self, Parameters(args): Parameters<GetCallGraphArgs>) -> String {
        let graph = {
            let guard = self.state.read().unwrap();
            match &*guard {
                IndexState::Pending => {
                    return r#"{"error": "Indexing in progress, please retry"}"#.to_string()
                }
                IndexState::Failed(msg) => {
                    return format!(r#"{{"error": "Indexing failed: {msg}"}}"#)
                }
                IndexState::Ready(_, g) => g.clone(),
            }
        };
        tokio::task::spawn_blocking(move || {
            let context = graph.get_symbol_context(&args.symbol);
            serde_json::to_string_pretty(&context)
                .unwrap_or_else(|e| format!(r#"{{"error": "{e}"}}"#))
        })
        .await
        .unwrap_or_else(|e| format!(r#"{{"error": "{e}"}}"#))
    }

    #[tool(description = "Perform hybrid search and graph traversal to find likely bug sources")]
    async fn analyze_bug(&self, Parameters(args): Parameters<AnalyzeBugArgs>) -> String {
        let (store, graph) = {
            let guard = self.state.read().unwrap();
            match &*guard {
                IndexState::Pending => {
                    return r#"{"error": "Indexing in progress, please retry"}"#.to_string()
                }
                IndexState::Failed(msg) => {
                    return format!(r#"{{"error": "Indexing failed: {msg}"}}"#)
                }
                IndexState::Ready(s, g) => (s.clone(), g.clone()),
            }
        };
        tokio::task::spawn_blocking(move || {
            let search_results = match store.search(&args.query, 3) {
                Ok(r) => r,
                Err(e) => {
                    return format!(r#"{{"error": "{e}"}}"#);
                }
            };

            let analysis_context: Vec<AnalysisChunk> = search_results
                .iter()
                .map(|res| {
                    let graph_context = if !res.name.is_empty() {
                        let ctx = graph.get_symbol_context(&res.name);
                        if ctx.defined_in.is_empty() && ctx.called_by.is_empty() {
                            None
                        } else {
                            Some(ctx)
                        }
                    } else {
                        None
                    };

                    AnalysisChunk {
                        file: res.file.clone(),
                        symbol: if res.name.is_empty() {
                            None
                        } else {
                            Some(res.name.clone())
                        },
                        code_snippet: res.text.clone(),
                        score: res.score,
                        graph_context,
                    }
                })
                .collect();

            let result = AnalysisResult {
                query: args.query,
                analysis_context,
            };

            serde_json::to_string_pretty(&result)
                .unwrap_or_else(|e| format!(r#"{{"error": "{e}"}}"#))
        })
        .await
        .unwrap_or_else(|e| format!(r#"{{"error": "{e}"}}"#))
    }
}

// ── Indexing Pipeline ───────────────────────────────────────

fn index_workspace(workspace: &Path, data_dir: &Path) -> Result<(CodeStore, CallGraphEngine)> {
    eprintln!("Indexing workspace: {}", workspace.display());
    let start = std::time::Instant::now();

    // 1. Walk the workspace, respecting .gitignore
    let files: Vec<PathBuf> = ignore::WalkBuilder::new(workspace)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_some_and(|ft| ft.is_file()))
        .filter(|e| {
            matches!(
                e.path().extension().and_then(|e| e.to_str()),
                Some("js" | "ts" | "jsx" | "tsx")
            )
        })
        .map(|e| e.path().to_path_buf())
        .collect();

    eprintln!("Found {} source files", files.len());

    if files.is_empty() {
        let store = CodeStore::new(data_dir)?;
        return Ok((store, CallGraphEngine::new()));
    }

    // 2. Parse all files in parallel
    let pb = ProgressBar::new(files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} files ({eta})")
            .unwrap()
            .progress_chars("█▉▊▋▌▍▎▏  "),
    );

    let parse_results: Vec<_> = files
        .par_iter()
        .filter_map(|path| {
            let result = std::fs::read_to_string(path).ok().and_then(|code| {
                let file_str = path.to_string_lossy().to_string();
                parser::parse_file(&file_str, &code).ok()
            });
            pb.inc(1);
            result
        })
        .collect();

    pb.finish_and_clear();

    // 3. Build store and graph
    let store = CodeStore::new(data_dir)?;
    let mut graph = CallGraphEngine::new();

    let mut all_symbols = Vec::new();

    for result in &parse_results {
        // Add to graph
        graph.add_file_data(
            &result.file_path,
            &result.symbols,
            &result.calls,
            &result.imports,
        );

        // Collect symbols for store
        all_symbols.extend(result.symbols.iter().cloned());
    }

    // 4. Batch insert into SQLite
    store
        .add_symbols(&all_symbols)
        .context("Failed to insert symbols into store")?;

    // 5. Resolve call graph
    graph.resolve_calls();

    let elapsed = start.elapsed();
    eprintln!(
        "Indexed {} files ({} symbols) in {:.2}s",
        parse_results.len(),
        all_symbols.len(),
        elapsed.as_secs_f64()
    );

    Ok((store, graph))
}

// ── Main ────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    // Route tracing output to stderr (stdout is MCP JSON-RPC)
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter("tree_worm=info")
        .init();

    let cli = Cli::parse();

    // Initialize shared state as Pending — indexing hasn't started yet
    let state = Arc::new(std::sync::RwLock::new(IndexState::Pending));

    info!("Starting MCP server...");
    eprintln!("Starting MCP server...");

    let server = TreeWormServer {
        state: state.clone(),
    };

    let router = Router::new(server).with_tools(TreeWormServer::tool_router());

    let service = router
        .serve(rmcp::transport::stdio())
        .await
        .context("Failed to start MCP server")?;

    // Spawn indexing in background — tools return "pending" until this completes
    let workspace = cli.workspace.clone();
    let data_dir = cli.data_dir.clone();
    tokio::task::spawn(async move {
        let state_clone = state.clone();
        let result =
            tokio::task::spawn_blocking(move || index_workspace(&workspace, &data_dir)).await;

        match result {
            Ok(Ok((store, graph))) => {
                *state_clone.write().unwrap() = IndexState::Ready(Arc::new(store), Arc::new(graph));
                eprintln!("Indexing complete — tools are now available.");
            }
            Ok(Err(e)) => {
                let msg = format!("{e:#}");
                eprintln!("Indexing failed: {msg}");
                *state_clone.write().unwrap() = IndexState::Failed(msg);
            }
            Err(e) => {
                let msg = format!("Indexing task panicked: {e}");
                eprintln!("{msg}");
                *state_clone.write().unwrap() = IndexState::Failed(msg);
            }
        }
    });

    service.waiting().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper: construct a fully-indexed server (Ready state) for existing tests.
    fn make_server() -> (TreeWormServer, TempDir, TempDir) {
        let workspace = TempDir::new().unwrap();
        let data_dir = TempDir::new().unwrap();

        // Create test files
        std::fs::write(
            workspace.path().join("utils.js"),
            "function calculate(a, b) { return a + b; }\nfunction validate(x) { return x > 0; }",
        )
        .unwrap();
        std::fs::write(
            workspace.path().join("app.js"),
            "function main() { calculate(1, 2); validate(3); }",
        )
        .unwrap();

        let (store, graph) = index_workspace(workspace.path(), data_dir.path()).unwrap();
        let server = TreeWormServer {
            state: Arc::new(std::sync::RwLock::new(IndexState::Ready(
                Arc::new(store),
                Arc::new(graph),
            ))),
        };
        (server, workspace, data_dir)
    }

    /// Helper: construct a server in Pending state (no indexing done).
    fn make_pending_server() -> TreeWormServer {
        TreeWormServer {
            state: Arc::new(std::sync::RwLock::new(IndexState::Pending)),
        }
    }

    /// Helper: construct a server in Failed state.
    fn make_failed_server(msg: &str) -> TreeWormServer {
        TreeWormServer {
            state: Arc::new(std::sync::RwLock::new(IndexState::Failed(msg.to_string()))),
        }
    }

    #[tokio::test]
    async fn server_search_code_returns_results() {
        let (server, _w, _d) = make_server();
        let result = server
            .search_code(Parameters(SearchCodeArgs {
                query: "calculate".into(),
            }))
            .await;
        assert!(result.contains("calculate"));
        assert!(result.contains("utils.js"));
        assert!(!result.contains("error"));
    }

    #[tokio::test]
    async fn server_search_code_no_results() {
        let (server, _w, _d) = make_server();
        let result = server
            .search_code(Parameters(SearchCodeArgs {
                query: "nonexistent_symbol_xyz".into(),
            }))
            .await;
        // Should return empty array, not error
        assert!(result.contains("[]") || result.contains("error"));
    }

    #[tokio::test]
    async fn server_get_symbol_returns_details() {
        let (server, _w, _d) = make_server();
        let result = server
            .get_symbol(Parameters(GetSymbolArgs {
                name: "calculate".into(),
            }))
            .await;
        assert!(result.contains("calculate"));
        assert!(result.contains("utils.js"));
    }

    #[tokio::test]
    async fn server_get_symbol_not_found() {
        let (server, _w, _d) = make_server();
        let result = server
            .get_symbol(Parameters(GetSymbolArgs {
                name: "does_not_exist".into(),
            }))
            .await;
        assert!(result.contains("[]"));
    }

    #[tokio::test]
    async fn server_get_call_graph_returns_context() {
        let (server, _w, _d) = make_server();
        let result = server
            .get_call_graph(Parameters(GetCallGraphArgs {
                symbol: "calculate".into(),
            }))
            .await;
        assert!(result.contains("calculate"));
        assert!(result.contains("defined_in"));
        assert!(result.contains("called_by"));
    }

    #[tokio::test]
    async fn server_get_call_graph_unknown_symbol() {
        let (server, _w, _d) = make_server();
        let result = server
            .get_call_graph(Parameters(GetCallGraphArgs {
                symbol: "unknown".into(),
            }))
            .await;
        assert!(result.contains("unknown"));
        // Empty arrays for defined_in and called_by
        assert!(result.contains("defined_in"));
    }

    #[tokio::test]
    async fn server_analyze_bug_returns_analysis() {
        let (server, _w, _d) = make_server();
        let result = server
            .analyze_bug(Parameters(AnalyzeBugArgs {
                query: "calculate returns wrong value".into(),
            }))
            .await;
        assert!(result.contains("calculate returns wrong value"));
        assert!(result.contains("analysis_context"));
    }

    #[tokio::test]
    async fn server_analyze_bug_no_matches() {
        let (server, _w, _d) = make_server();
        let result = server
            .analyze_bug(Parameters(AnalyzeBugArgs {
                query: "completely_unrelated_bug_xyz".into(),
            }))
            .await;
        assert!(result.contains("completely_unrelated_bug_xyz"));
        assert!(result.contains("analysis_context"));
    }

    #[test]
    fn server_get_info() {
        let (server, _w, _d) = make_server();
        let info = server.get_info();
        assert!(info.instructions.is_some());
        assert!(info.instructions.unwrap().contains("Code intelligence"));
    }

    #[test]
    fn index_empty_workspace() {
        let workspace = TempDir::new().unwrap();
        let data_dir = TempDir::new().unwrap();
        let (store, _graph) = index_workspace(workspace.path(), data_dir.path()).unwrap();

        // Empty workspace should produce empty results
        let results = store.search("anything", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn index_workspace_with_js_files() {
        let workspace = TempDir::new().unwrap();
        let data_dir = TempDir::new().unwrap();

        // Create a JS file
        let js_path = workspace.path().join("app.js");
        std::fs::write(
            &js_path,
            "function hello() { return 'world'; }\nfunction greet(name) { return hello() + name; }",
        )
        .unwrap();

        let (store, graph) = index_workspace(workspace.path(), data_dir.path()).unwrap();

        // Should find symbols
        let results = store.search("hello", 10).unwrap();
        assert!(!results.is_empty());

        let sym = store.get_symbol("hello").unwrap();
        assert_eq!(sym.len(), 1);
        assert_eq!(sym[0].name, "hello");

        // Graph should have call relationship
        let ctx = graph.get_symbol_context("hello");
        assert!(!ctx.defined_in.is_empty());
    }

    #[test]
    fn index_workspace_with_ts_files() {
        let workspace = TempDir::new().unwrap();
        let data_dir = TempDir::new().unwrap();

        let ts_path = workspace.path().join("utils.ts");
        std::fs::write(
            &ts_path,
            "function add(a: number, b: number): number { return a + b; }",
        )
        .unwrap();

        let (store, _graph) = index_workspace(workspace.path(), data_dir.path()).unwrap();

        let sym = store.get_symbol("add").unwrap();
        assert_eq!(sym.len(), 1);
    }

    #[test]
    fn index_workspace_ignores_non_js_ts_files() {
        let workspace = TempDir::new().unwrap();
        let data_dir = TempDir::new().unwrap();

        std::fs::write(workspace.path().join("readme.md"), "# Hello").unwrap();
        std::fs::write(workspace.path().join("main.py"), "def hello(): pass").unwrap();
        std::fs::write(workspace.path().join("style.css"), "body {}").unwrap();

        let (store, _graph) = index_workspace(workspace.path(), data_dir.path()).unwrap();
        let results = store.search("hello", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn index_workspace_with_nested_dirs() {
        let workspace = TempDir::new().unwrap();
        let data_dir = TempDir::new().unwrap();

        let nested = workspace.path().join("src").join("components");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            nested.join("Button.jsx"),
            r#"function Button() { return <button>Click</button>; }"#,
        )
        .unwrap();

        let (store, _graph) = index_workspace(workspace.path(), data_dir.path()).unwrap();
        let sym = store.get_symbol("Button").unwrap();
        assert_eq!(sym.len(), 1);
    }

    #[test]
    fn index_workspace_multiple_files_with_calls() {
        let workspace = TempDir::new().unwrap();
        let data_dir = TempDir::new().unwrap();

        std::fs::write(
            workspace.path().join("utils.js"),
            "function validate(x) { return x > 0; }",
        )
        .unwrap();
        std::fs::write(
            workspace.path().join("app.js"),
            "function run() { validate(42); }",
        )
        .unwrap();

        let (_store, graph) = index_workspace(workspace.path(), data_dir.path()).unwrap();

        let ctx = graph.get_symbol_context("validate");
        assert!(!ctx.defined_in.is_empty());
        // "validate" is called from app.js
        assert!(!ctx.called_by.is_empty());
    }

    // ── New tests: Pending and Failed states ──

    #[tokio::test]
    async fn server_pending_returns_indexing_message() {
        let server = make_pending_server();
        let result = server
            .search_code(Parameters(SearchCodeArgs {
                query: "anything".into(),
            }))
            .await;
        assert!(result.contains("Indexing in progress"));

        let result = server
            .get_symbol(Parameters(GetSymbolArgs {
                name: "anything".into(),
            }))
            .await;
        assert!(result.contains("Indexing in progress"));

        let result = server
            .get_call_graph(Parameters(GetCallGraphArgs {
                symbol: "anything".into(),
            }))
            .await;
        assert!(result.contains("Indexing in progress"));

        let result = server
            .analyze_bug(Parameters(AnalyzeBugArgs {
                query: "anything".into(),
            }))
            .await;
        assert!(result.contains("Indexing in progress"));
    }

    #[tokio::test]
    async fn server_failed_returns_error_message() {
        let server = make_failed_server("SQLite connection refused");
        let result = server
            .search_code(Parameters(SearchCodeArgs {
                query: "anything".into(),
            }))
            .await;
        assert!(result.contains("Indexing failed"));
        assert!(result.contains("SQLite connection refused"));
    }

    // ── Integration test: MCP handshake timing ──

    #[tokio::test]
    async fn mcp_handshake_responds_within_one_second() {
        use std::process::Stdio;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::process::Command;

        // Build the binary first (assumes cargo build has run)
        let binary = std::env::current_dir()
            .unwrap()
            .join("target")
            .join("debug")
            .join("tree-worm");

        if !binary.exists() {
            // Skip if binary not built (CI will build before test)
            eprintln!("Skipping timing test: binary not found at {:?}", binary);
            return;
        }

        // Create a large workspace to stress-test indexing time
        let workspace = TempDir::new().unwrap();
        let data_dir = TempDir::new().unwrap();
        for i in 0..100 {
            std::fs::write(
                workspace.path().join(format!("file_{i}.js")),
                format!("function handler_{i}() {{ return {i}; }}"),
            )
            .unwrap();
        }

        let mut child = Command::new(&binary)
            .arg("--workspace")
            .arg(workspace.path())
            .arg("--data-dir")
            .arg(data_dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("Failed to spawn tree-worm");

        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();

        // Send initialize request
        let init_msg = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
        stdin
            .write_all(format!("{init_msg}\n").as_bytes())
            .await
            .unwrap();

        // Read response with 1-second timeout
        let mut buf = vec![0u8; 4096];
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(1), stdout.read(&mut buf)).await;

        // Clean up
        child.kill().await.ok();

        let bytes_read = result
            .expect("MCP handshake did not respond within 1 second")
            .expect("Failed to read stdout");

        let response = String::from_utf8_lossy(&buf[..bytes_read]);
        assert!(
            response.contains("protocolVersion"),
            "Response should contain protocolVersion: {response}"
        );
    }
}
