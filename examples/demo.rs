use std::path::PathBuf;

// Copy our functions here for the demo
// (In a real project, we'd expose these from lib.rs)

fn view_name_to_path(view_name: &str) -> PathBuf {
    let mut path = PathBuf::from("resources/views");
    for segment in view_name.split('.') {
        path.push(segment);
    }
    path.set_extension("blade.php");
    path
}

fn find_view_calls(php_content: &str) -> Vec<String> {
    let mut views = Vec::new();

    for line in php_content.lines() {
        if let Some(start_pos) = line.find("view(") {
            let after_view = &line[start_pos + 5..];

            if let Some(quote_start) = after_view.find(|c| c == '\'' || c == '"') {
                let quote_char = after_view.chars().nth(quote_start).unwrap();
                let after_quote = &after_view[quote_start + 1..];

                if let Some(quote_end) = after_quote.find(quote_char) {
                    let view_name = &after_quote[..quote_end];
                    views.push(view_name.to_string());
                }
            }
        }
    }

    views
}

fn main() {
    println!("🚀 Laravel Extension - Phase 2 Demo");
    println!("====================================\n");

    // Demo 1: View name to path conversion
    println!("📁 View Name to Path Conversion:");
    println!("---------------------------------");

    let test_views = vec![
        "welcome",
        "users.profile",
        "admin.dashboard.index",
        "emails.order.confirmation",
    ];

    for view in test_views {
        let path = view_name_to_path(view);
        println!("  '{}' → {}", view, path.display());
    }

    println!();

    // Demo 2: Finding view calls in PHP code
    println!("🔍 Finding View Calls in PHP Code:");
    println!("-----------------------------------");

    let sample_php = r#"
<?php

namespace App\Http\Controllers;

use App\Models\User;
use Illuminate\Http\Request;

class UserController extends Controller
{
    public function index()
    {
        $users = User::all();
        return view('users.index', compact('users'));
    }
    
    public function show(User $user)
    {
        return view('users.profile', [
            'user' => $user
        ]);
    }
    
    public function dashboard()
    {
        // Admin dashboard
        return view("admin.dashboard");
    }
    
    public function settings()
    {
        return view('users.settings')->with('title', 'Settings');
    }
}
    "#;

    println!("PHP Code Sample:");
    println!("{}", sample_php);

    println!("\nFound Laravel Views:");
    let found_views = find_view_calls(sample_php);
    for (i, view) in found_views.iter().enumerate() {
        let path = view_name_to_path(view);
        println!("  {}. '{}' → {}", i + 1, view, path.display());
    }

    println!();
    println!("✅ Phase 2 Complete: File System Navigation");
    println!("   - Parse Laravel view names ✓");
    println!("   - Convert to file paths ✓");
    println!("   - Find view calls in PHP code ✓");
    println!();
    println!("📚 Rust concepts learned:");
    println!("   - String vs &str types");
    println!("   - PathBuf for file paths");
    println!("   - Vec<T> for collections");
    println!("   - Pattern matching with if let");
    println!("   - Option<T> for nullable values");
}
