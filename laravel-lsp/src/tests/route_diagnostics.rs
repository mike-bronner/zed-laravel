use crate::LaravelLanguageServer;
use laravel_lsp::route_discovery::{RouteDefinition, RouteIndex};
use laravel_lsp::salsa_impl::RouteReferenceData;
use std::path::PathBuf;
use std::sync::Arc;
use tower_lsp::lsp_types::DiagnosticSeverity;

fn route_ref(name: &str) -> Arc<RouteReferenceData> {
    Arc::new(RouteReferenceData {
        name: name.to_string(),
        line: 0,
        column: 0,
        end_column: 5,
    })
}

fn index_with(names: &[&str]) -> RouteIndex {
    let mut idx = RouteIndex::new();
    for n in names {
        idx.insert(
            n.to_string(),
            RouteDefinition {
                file: PathBuf::from("routes/web.php"),
                line: 0,
                column: 0,
                end_column: 0,
                priority: 0,
                method: None,
                uri: None,
                action: None,
            },
        );
    }
    idx
}

#[test]
fn flags_only_unknown_routes() {
    let idx = index_with(&["home", "admin.dashboard"]);
    let refs = vec![route_ref("home"), route_ref("does.not.exist")];
    let diags = LaravelLanguageServer::route_not_found_diagnostics(Some(&idx), &refs);
    assert_eq!(diags.len(), 1, "only the unknown route should be flagged");
    assert!(diags[0].message.contains("does.not.exist"));
    assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
}

#[test]
fn empty_index_flags_nothing() {
    // Guard: before the index is built (empty), we must NOT flag every route —
    // that would be a false-positive storm at startup.
    let idx = RouteIndex::new();
    let diags = LaravelLanguageServer::route_not_found_diagnostics(Some(&idx), &[route_ref("x")]);
    assert!(diags.is_empty());
}

#[test]
fn absent_index_flags_nothing() {
    let diags = LaravelLanguageServer::route_not_found_diagnostics(None, &[route_ref("x")]);
    assert!(diags.is_empty());
}

#[test]
fn all_known_routes_flag_nothing() {
    let idx = index_with(&["home"]);
    let diags =
        LaravelLanguageServer::route_not_found_diagnostics(Some(&idx), &[route_ref("home")]);
    assert!(diags.is_empty());
}
