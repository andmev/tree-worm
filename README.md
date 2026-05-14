<p align="center">
  <img src="logo.png" alt="Tree-Worm Logo" width="300" />
</p>

# Tree-Worm: Local Code Intelligence MCP Server

[![npm version](https://img.shields.io/npm/v/tree-worm?color=cb0000&label=npm)](https://www.npmjs.com/package/tree-worm)
[![npm downloads](https://img.shields.io/npm/dm/tree-worm?color=cb0000)](https://www.npmjs.com/package/tree-worm)
[![CI](https://github.com/andmev/tree-worm/actions/workflows/ci.yml/badge.svg)](https://github.com/andmev/tree-worm/actions/workflows/ci.yml)
[![Release](https://github.com/andmev/tree-worm/actions/workflows/release.yml/badge.svg)](https://github.com/andmev/tree-worm/actions/workflows/release.yml)
[![codecov](https://codecov.io/gh/andmev/tree-worm/branch/main/graph/badge.svg)](https://codecov.io/gh/andmev/tree-worm)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A fast, native code intelligence server using Tree-sitter, SQLite FTS5, and Call Graphs — distributed as a single binary via npm. Connects to any MCP client (Claude Desktop, Cursor, VS Code, Pi, OpenCode) to give AI agents structural understanding of your codebase.

## The Problem

AI coding assistants are powerful, but they are blind to your codebase. They can't find where a function is defined, who calls it, or how modules relate to each other. Without structural understanding, they hallucinate symbol names, miss critical call chains, and give advice that doesn't fit the code that actually exists.

Tree-Worm solves this by building a local, private code intelligence layer. It parses your codebase with Tree-sitter to extract real AST structure, indexes symbols into SQLite FTS5 for fast keyword search, and constructs an in-memory call graph so relationships between symbols are queryable. All of this is exposed as an MCP server over stdio — giving any AI agent the ability to search, navigate, and reason about your code the way a senior engineer would.

## Quick Start

```bash
npx tree-worm
```

That's it. The binary indexes your workspace on startup and starts the MCP server.

### MCP Client Configuration

Add to your MCP client config (Claude Desktop, Cursor, Pi, OpenCode):

```json
{
  "mcpServers": {
    "tree-worm": {
      "command": "npx",
      "args": ["-y", "tree-worm"]
    }
  }
}
```

### CLI Options

```
tree-worm [OPTIONS]

Options:
  -w, --workspace <PATH>   Workspace directory to index [default: .]
  -d, --data-dir <PATH>    Data directory for the search index [default: .tree-worm]
  -h, --help               Print help
  -V, --version            Print version
```

## Architecture

* **Tree-sitter Parsing** — Parses JavaScript and TypeScript files (`.js`, `.ts`, `.jsx`, `.tsx`) into structural AST units: functions, classes, arrow functions, imports, and call expressions.
* **SQLite FTS5** — BM25-ranked full-text search over symbol names and code. Zero model downloads, sub-millisecond queries, single `.db` file.
* **Call Graph Engine** — In-memory directed graph (petgraph) connecting function definitions to call sites. Rebuilt on every startup.
* **MCP Server** — Stdio-based JSON-RPC server using the official Rust MCP SDK (rmcp). Auto-generates tool schemas.

### What happens on startup

1. Walks the workspace respecting `.gitignore` (via the `ignore` crate)
2. Parses all JS/TS files in parallel (rayon)
3. Inserts symbols into SQLite FTS5
4. Builds and resolves the call graph
5. Starts the MCP stdio server

Typical indexing time: **<1s** for most projects, **<5s** for large monorepos.

## Available MCP Tools

| Tool | Description |
|------|-------------|
| `search_code` | BM25-ranked keyword search over the indexed repository |
| `get_symbol` | Look up a specific symbol by exact name — returns code, file path, and line numbers |
| `get_call_graph` | Get where a symbol is defined and who calls it |
| `analyze_bug` | Hybrid search + graph traversal to find likely bug sources |

### Example Usage

```
> search_code("authentication middleware")
[{ file: "src/auth.ts", name: "authMiddleware", start_line: 12, ... }]

> get_symbol("authMiddleware")
[{ file: "src/auth.ts", type: "function_declaration", code: "...", ... }]

> get_call_graph("authMiddleware")
{ symbol: "authMiddleware", defined_in: ["src/auth.ts"], called_by: ["src/app.ts", "src/routes.ts"] }

> analyze_bug("login fails with 401 after token refresh")
{ query: "...", analysis_context: [{ file: "src/auth.ts", symbol: "refreshToken", graph_context: {...} }] }
```

## Supported Languages

- JavaScript (`.js`)
- TypeScript (`.ts`)
- JSX (`.jsx`)
- TSX (`.tsx`)

## Platform Support

Distributed as platform-specific npm packages via `optionalDependencies`:

| Platform | Package |
|----------|---------|
| macOS ARM64 | `tree-worm-darwin-arm64` |
| macOS x64 | `tree-worm-darwin-x64` |
| Linux x64 | `tree-worm-linux-x64` |
| Linux ARM64 | `tree-worm-linux-arm64` |
| Linux x64 (musl) | `tree-worm-linux-x64-musl` |
| Windows x64 | `tree-worm-win32-x64` |

### Using a local binary

Set the `TREE_WORM_BINARY` environment variable to bypass npm package resolution:

```bash
TREE_WORM_BINARY=./target/release/tree-worm npx tree-worm
```

## Building from Source

```bash
# Build
cargo build --release

# Run
./target/release/tree-worm --workspace /path/to/project
```

### Requirements

- Rust 1.70+
- No runtime dependencies — SQLite is bundled, tree-sitter grammars are compiled in

## License

MIT
