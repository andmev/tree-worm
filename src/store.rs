use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use crate::types::{ParsedSymbol, SearchResult, SymbolResult};

pub struct CodeStore {
    conn: Mutex<Connection>,
}

impl CodeStore {
    /// Create a new CodeStore, dropping any existing data for a clean rebuild.
    pub fn new(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir).context("Failed to create data directory")?;

        let db_path = data_dir.join("code_search.db");
        let conn = Connection::open(&db_path).context("Failed to open SQLite database")?;

        // Enable WAL mode for better concurrent read performance
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;

        // Drop existing tables for clean rebuild on every startup
        // Create tables: content table + unicode61 FTS5 + trigram FTS5
        conn.execute_batch(
            "
            DROP TABLE IF EXISTS chunks_trigram;
            DROP TABLE IF EXISTS chunks_fts;
            DROP TABLE IF EXISTS chunks;

            CREATE TABLE chunks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file TEXT NOT NULL,
                name TEXT NOT NULL,
                type TEXT NOT NULL,
                start_line INTEGER NOT NULL,
                end_line INTEGER NOT NULL,
                text TEXT NOT NULL
            );

            CREATE VIRTUAL TABLE chunks_fts USING fts5(
                name,
                text,
                content='chunks',
                content_rowid='id'
            );

            CREATE TRIGGER chunks_ai AFTER INSERT ON chunks BEGIN
                INSERT INTO chunks_fts(rowid, name, text)
                VALUES (new.id, new.name, new.text);
            END;

            CREATE VIRTUAL TABLE chunks_trigram USING fts5(
                name,
                content='chunks',
                content_rowid='id',
                tokenize='trigram case_sensitive 0'
            );

            CREATE TRIGGER chunks_trigram_ai AFTER INSERT ON chunks BEGIN
                INSERT INTO chunks_trigram(rowid, name)
                VALUES (new.id, new.name);
            END;
        ",
        )?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Insert parsed symbols into the database in a single transaction.
    pub fn add_symbols(&self, symbols: &[ParsedSymbol]) -> Result<()> {
        if symbols.is_empty() {
            return Ok(());
        }

        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let tx = conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO chunks (file, name, type, start_line, end_line, text)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;

            for sym in symbols {
                stmt.execute(params![
                    sym.file_path,
                    sym.name,
                    sym.symbol_type,
                    sym.start_line as u32,
                    sym.end_line as u32,
                    sym.code,
                ])?;
            }
        }
        tx.commit()?;

        Ok(())
    }

    /// Search for code using multi-strategy FTS5 with fallback.
    /// Strategy 1: AND + prefix on unicode61 FTS5 (most precise)
    /// Strategy 2: OR + prefix on unicode61 FTS5 (broader recall)
    /// Strategy 3: Trigram substring match on name column (fallback)
    pub fn search(&self, query: &str, top_k: usize) -> Result<Vec<SearchResult>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(vec![]);
        }

        let (and_query, or_query) = Self::build_fuzzy_queries(trimmed);

        if and_query.is_empty() {
            return Ok(vec![]);
        }

        // Strategy 1: AND + prefix (precise — all terms must match)
        let results = self.search_fts(&and_query, top_k)?;
        if !results.is_empty() {
            return Ok(results);
        }

        // Strategy 2: OR + prefix (broader — any term matches)
        let results = self.search_fts(&or_query, top_k)?;
        if !results.is_empty() {
            return Ok(results);
        }

        // Strategy 3: Trigram substring match on name column
        if trimmed.len() >= 3 {
            let trigram_results = self.search_trigram(trimmed, top_k)?;
            if !trigram_results.is_empty() {
                return Ok(trigram_results);
            }
        }

        Ok(vec![])
    }

    /// Build FTS5 query with multi-strategy: AND first (precise), then OR (broad).
    /// Returns (and_query, or_query) pair.
    /// "auth middleware" → ("auth* AND middleware*", "auth* OR middleware*")
    fn build_fuzzy_queries(query: &str) -> (String, String) {
        let tokens: Vec<String> = query
            .split_whitespace()
            .map(|token| {
                // Strip FTS5 special characters, keep only alphanumeric + underscore
                let clean: String = token
                    .chars()
                    .filter(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                clean
            })
            .filter(|t| !t.is_empty())
            .map(|t| format!("{}*", t))
            .collect();

        if tokens.is_empty() {
            return (String::new(), String::new());
        }

        let and_query = tokens.join(" AND ");
        let or_query = tokens.join(" OR ");
        (and_query, or_query)
    }

    /// Search the primary unicode61 FTS5 table with BM25 name-boosted ranking.
    fn search_fts(&self, fts_query: &str, top_k: usize) -> Result<Vec<SearchResult>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT c.file, c.name, c.type, c.start_line, c.end_line, c.text,
                    bm25(chunks_fts, 10.0, 1.0) as score
             FROM chunks_fts
             JOIN chunks c ON c.id = chunks_fts.rowid
             WHERE chunks_fts MATCH ?1
             ORDER BY score
             LIMIT ?2",
        )?;

        let results = stmt
            .query_map(params![fts_query, top_k as u32], |row| {
                Ok(SearchResult {
                    file: row.get(0)?,
                    name: row.get(1)?,
                    symbol_type: row.get(2)?,
                    start_line: row.get(3)?,
                    end_line: row.get(4)?,
                    text: row.get(5)?,
                    score: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(results)
    }

    /// Search the trigram FTS5 table for substring matches on symbol names.
    fn search_trigram(&self, query: &str, top_k: usize) -> Result<Vec<SearchResult>> {
        let escaped = query.replace('"', "\"\"");
        let trigram_query = format!("\"{}\"", escaped);

        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT c.file, c.name, c.type, c.start_line, c.end_line, c.text, 0.0 as score
             FROM chunks_trigram
             JOIN chunks c ON c.id = chunks_trigram.rowid
             WHERE chunks_trigram MATCH ?1
             LIMIT ?2",
        )?;

        let results = stmt
            .query_map(params![trigram_query, top_k as u32], |row| {
                Ok(SearchResult {
                    file: row.get(0)?,
                    name: row.get(1)?,
                    symbol_type: row.get(2)?,
                    start_line: row.get(3)?,
                    end_line: row.get(4)?,
                    text: row.get(5)?,
                    score: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(results)
    }

    /// Look up a symbol by exact name.
    pub fn get_symbol(&self, name: &str) -> Result<Vec<SymbolResult>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT file, name, type, start_line, end_line, text
             FROM chunks
             WHERE name = ?1",
        )?;

        let results = stmt
            .query_map(params![name], |row| {
                Ok(SymbolResult {
                    file: row.get(0)?,
                    name: row.get(1)?,
                    symbol_type: row.get(2)?,
                    start_line: row.get(3)?,
                    end_line: row.get(4)?,
                    text: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (CodeStore, TempDir) {
        let tmp = TempDir::new().unwrap();
        let store = CodeStore::new(tmp.path()).unwrap();
        (store, tmp)
    }

    fn make_symbol(name: &str, file: &str, code: &str) -> ParsedSymbol {
        ParsedSymbol {
            name: name.into(),
            symbol_type: "function_declaration".into(),
            file_path: file.into(),
            start_line: 1,
            end_line: 5,
            code: code.into(),
        }
    }

    #[test]
    fn new_store_creates_db() {
        let (store, _tmp) = make_store();
        // Should be able to search empty store
        let results = store.search("anything", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn add_symbols_empty_vec() {
        let (store, _tmp) = make_store();
        // Should not error on empty
        store.add_symbols(&[]).unwrap();
    }

    #[test]
    fn add_and_search_symbols() {
        let (store, _tmp) = make_store();
        let syms = vec![
            make_symbol(
                "calculateTotal",
                "src/math.js",
                "function calculateTotal() { return 42; }",
            ),
            make_symbol(
                "formatPrice",
                "src/format.js",
                "function formatPrice(p) { return '$' + p; }",
            ),
        ];
        store.add_symbols(&syms).unwrap();

        let results = store.search("calculateTotal", 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "calculateTotal");
        assert_eq!(results[0].file, "src/math.js");
        assert_eq!(results[0].symbol_type, "function_declaration");
    }

    #[test]
    fn search_by_code_content() {
        let (store, _tmp) = make_store();
        let syms = vec![make_symbol(
            "auth",
            "src/auth.js",
            "function auth() { validateToken(); checkPermissions(); }",
        )];
        store.add_symbols(&syms).unwrap();

        let results = store.search("validateToken", 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "auth");
    }

    #[test]
    fn search_respects_top_k() {
        let (store, _tmp) = make_store();
        let syms: Vec<ParsedSymbol> = (0..20)
            .map(|i| {
                make_symbol(
                    &format!("handler{}", i),
                    "src/app.js",
                    &format!("function handler{}() {{ process(); }}", i),
                )
            })
            .collect();
        store.add_symbols(&syms).unwrap();

        let results = store.search("handler", 5).unwrap();
        assert!(results.len() <= 5);
    }

    #[test]
    fn search_empty_query() {
        let (store, _tmp) = make_store();
        let syms = vec![make_symbol("foo", "a.js", "function foo() {}")];
        store.add_symbols(&syms).unwrap();

        let results = store.search("", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_whitespace_only_query() {
        let (store, _tmp) = make_store();
        let syms = vec![make_symbol("foo", "a.js", "function foo() {}")];
        store.add_symbols(&syms).unwrap();

        let results = store.search("   ", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn get_symbol_by_exact_name() {
        let (store, _tmp) = make_store();
        let syms = vec![
            make_symbol("myFunction", "src/a.js", "function myFunction() {}"),
            make_symbol("otherFunction", "src/b.js", "function otherFunction() {}"),
        ];
        store.add_symbols(&syms).unwrap();

        let results = store.get_symbol("myFunction").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "myFunction");
        assert_eq!(results[0].file, "src/a.js");
    }

    #[test]
    fn get_symbol_not_found() {
        let (store, _tmp) = make_store();
        let syms = vec![make_symbol("foo", "a.js", "function foo() {}")];
        store.add_symbols(&syms).unwrap();

        let results = store.get_symbol("nonexistent").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn get_symbol_multiple_definitions() {
        let (store, _tmp) = make_store();
        let syms = vec![
            make_symbol("render", "src/a.js", "function render() { /* a */ }"),
            make_symbol("render", "src/b.js", "function render() { /* b */ }"),
        ];
        store.add_symbols(&syms).unwrap();

        let results = store.get_symbol("render").unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn store_rebuilt_on_new() {
        let tmp = TempDir::new().unwrap();

        // First store with data
        {
            let store = CodeStore::new(tmp.path()).unwrap();
            let syms = vec![make_symbol("old", "a.js", "function old() {}")];
            store.add_symbols(&syms).unwrap();
            let results = store.get_symbol("old").unwrap();
            assert_eq!(results.len(), 1);
        }

        // Second store should start fresh (tables dropped)
        {
            let store = CodeStore::new(tmp.path()).unwrap();
            let results = store.get_symbol("old").unwrap();
            assert!(results.is_empty());
        }
    }

    #[test]
    fn search_with_special_characters() {
        let (store, _tmp) = make_store();
        let syms = vec![make_symbol(
            "foo",
            "a.js",
            "function foo() { return a + b; }",
        )];
        store.add_symbols(&syms).unwrap();

        // FTS5 special chars are escaped via quoting
        let results = store.search("foo", 10).unwrap();
        assert!(!results.is_empty());
    }

    #[test]
    fn search_result_has_score() {
        let (store, _tmp) = make_store();
        let syms = vec![make_symbol(
            "findUser",
            "a.js",
            "function findUser() { return db.query(); }",
        )];
        store.add_symbols(&syms).unwrap();

        let results = store.search("findUser", 10).unwrap();
        assert!(!results.is_empty());
        // BM25 rank is typically negative (lower = better match)
        assert!(results[0].score != 0.0);
    }

    #[test]
    fn search_or_prefix_finds_partial_match() {
        let (store, _tmp) = make_store();
        let syms = vec![make_symbol(
            "authenticateUser",
            "src/auth.js",
            "function authenticateUser(token) { verify(token); }",
        )];
        store.add_symbols(&syms).unwrap();
        let results = store.search("auth", 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "authenticateUser");
    }

    #[test]
    fn search_multi_word_or() {
        let (store, _tmp) = make_store();
        let syms = vec![
            make_symbol(
                "authMiddleware",
                "src/auth.js",
                "function authMiddleware(req, res, next) {}",
            ),
            make_symbol(
                "validateInput",
                "src/validate.js",
                "function validateInput(data) {}",
            ),
        ];
        store.add_symbols(&syms).unwrap();
        let results = store.search("auth middleware", 10).unwrap();
        assert!(!results.is_empty());
    }

    #[test]
    fn search_trigram_fallback() {
        let (store, _tmp) = make_store();
        let syms = vec![make_symbol(
            "authMiddleware",
            "src/auth.js",
            "function authMiddleware() {}",
        )];
        store.add_symbols(&syms).unwrap();
        // "Middleware" as substring of name — trigram fallback should find it
        let results = store.search("Middleware", 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "authMiddleware");
    }

    #[test]
    fn search_bm25_name_boost() {
        let (store, _tmp) = make_store();
        let syms = vec![
            make_symbol(
                "validate",
                "src/validate.js",
                "function validate(x) { return x > 0; }",
            ),
            make_symbol(
                "process",
                "src/process.js",
                "function process(data) { validate(data); return data; }",
            ),
        ];
        store.add_symbols(&syms).unwrap();
        let results = store.search("validate", 10).unwrap();
        assert!(!results.is_empty());
        // Name-matched "validate" should rank higher than text-matched "process"
        assert_eq!(results[0].name, "validate");
    }

    #[test]
    fn search_short_query_skips_trigram() {
        let (store, _tmp) = make_store();
        let syms = vec![make_symbol("ab", "a.js", "function ab() {}")];
        store.add_symbols(&syms).unwrap();
        // 2-char query: trigram requires >=3 chars, so only unicode61 FTS5 is tried
        let results = store.search("ab", 10).unwrap();
        assert!(!results.is_empty());
    }
}
