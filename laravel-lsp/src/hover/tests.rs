use super::*;
use crate::php_class::PropertyDeclaration;
use crate::route_discovery::{RouteDefinition, PRIORITY_APP};
use std::path::PathBuf;

// All hovers share an intelephense-style shape:
//   **<bold header — call form>**
//   [optional detail / description]
//   [optional fenced code block — value or declaration]
//   at <source path>
//   [optional *(trailer)*]
//
// Tests assert on user-visible substrings.

// ============================================================================
// View / component / Livewire
// ============================================================================

#[test]
fn view_hover_uses_call_form_header() {
    let out = format_view(
        "users.profile",
        Some("resources/views/users/profile.blade.php"),
        None,
    );
    assert!(out.contains("**view('users.profile')**"));
    assert!(out.contains("at resources/views/users/profile.blade.php"));
}

#[test]
fn view_hover_with_snippet_renders_php_block() {
    let out = format_view(
        "users.profile",
        Some("resources/views/users/profile.blade.php"),
        Some("@props(['user' => null])"),
    );
    assert!(out.contains("```php"));
    assert!(out.contains("@props(['user' => null])"));
}

#[test]
fn view_hover_without_path_shows_missing_trailer() {
    let out = format_view("users.missing", None, None);
    assert!(out.contains("**view('users.missing')**"));
    assert!(out.contains("*(file not found)*"));
}

#[test]
fn component_hover_does_not_double_prefix() {
    let out = format_component(
        "x-button",
        Some("resources/views/components/button.blade.php"),
        None,
    );
    assert!(out.contains("`<x-button>`"));
    assert!(!out.contains("<x-x-"));
}

#[test]
fn livewire_hover_uses_tag_form_header() {
    let out = format_livewire("counter", Some("app/Livewire/Counter.php"), None);
    assert!(out.contains("`<livewire:counter>`"));
    assert!(out.contains("at app/Livewire/Counter.php"));
}

#[test]
fn livewire_hover_renders_class_signature_snippet() {
    let out = format_livewire(
        "counter",
        Some("app/Livewire/Counter.php"),
        Some("class Counter extends Component"),
    );
    assert!(out.contains("```php"));
    assert!(out.contains("class Counter extends Component"));
}

// ============================================================================
// Route
// ============================================================================

fn route_def(method: &str, uri: &str, action: &str) -> RouteDefinition {
    RouteDefinition {
        file: PathBuf::from("/fake/routes/web.php"),
        line: 0,
        column: 0,
        end_column: 0,
        priority: PRIORITY_APP,
        method: Some(method.to_string()),
        uri: Some(uri.to_string()),
        action: Some(action.to_string()),
    }
}

#[test]
fn route_hover_shows_method_uri_action() {
    let def = route_def("get", "/users/{user}", "UserController@show");
    let out = format_route("users.show", Some(&def), Some("routes/web.php:42"), None);
    assert!(out.contains("**route('users.show')**"));
    assert!(out.contains("`GET /users/{user}`"));
    assert!(out.contains("`UserController@show`"));
    assert!(out.contains("at routes/web.php:42"));
}

#[test]
fn route_hover_renders_source_line_snippet() {
    let def = route_def("get", "/users/{user}", "UserController@show");
    let snippet =
        "Route::get('/users/{user}', [UserController::class, 'show'])->name('users.show');";
    let out = format_route(
        "users.show",
        Some(&def),
        Some("routes/web.php:42"),
        Some(snippet),
    );
    assert!(out.contains("```php"));
    assert!(out.contains(snippet));
}

#[test]
fn route_hover_handles_missing_index_entry() {
    let out = format_route("orphan", None, None, None);
    assert!(out.contains("**route('orphan')**"));
    assert!(out.contains("*(route not found in index)*"));
}

// ============================================================================
// Config
// ============================================================================

#[test]
fn config_hover_shows_value_in_php_block() {
    let out = format_config(
        "app.name",
        Some("env('APP_NAME', 'Laravel')"),
        Some("config/app.php"),
    );
    assert!(out.contains("**config('app.name')**"));
    assert!(out.contains("```php"));
    assert!(out.contains("env('APP_NAME', 'Laravel')"));
    assert!(out.contains("at config/app.php"));
}

#[test]
fn config_hover_without_value() {
    let out = format_config("app.unknown", None, Some("config/app.php"));
    assert!(out.contains("**config('app.unknown')**"));
    assert!(out.contains("*(value not found)*"));
}

#[test]
fn config_hover_truncates_long_values() {
    let long = "x".repeat(500);
    let out = format_config("some.key", Some(&long), None);
    assert!(out.contains('…'));
    assert_eq!(
        out.chars().filter(|c| *c == 'x').count(),
        200,
        "200 char cap"
    );
}

// ============================================================================
// Env
// ============================================================================

fn env_input<'a>(value: &'a str, source_file: Option<&'a str>) -> EnvHoverInput<'a> {
    EnvHoverInput {
        value,
        is_commented: false,
        source_file,
    }
}

#[test]
fn env_hover_shows_value_in_plain_block() {
    let out = format_env("APP_NAME", Some(env_input("Laravel", Some(".env"))));
    assert!(out.contains("**env('APP_NAME')**"));
    // Plain ``` (no `php` lang) — env values aren't PHP code.
    assert!(out.contains("```\nLaravel\n```"));
    assert!(out.contains("at .env"));
}

#[test]
fn env_hover_shows_secret_value_plainly() {
    let out = format_env(
        "APP_KEY",
        Some(env_input("base64:abcdef1234567890", Some(".env"))),
    );
    assert!(out.contains("base64:abcdef1234567890"));
    assert!(!out.contains("•"));
    assert!(!out.contains("masked"));
}

#[test]
fn env_hover_marks_commented_value() {
    let out = format_env(
        "APP_DEBUG",
        Some(EnvHoverInput {
            value: "true",
            is_commented: true,
            source_file: Some(".env"),
        }),
    );
    assert!(out.contains("*(commented out)*"));
    assert!(out.contains(".env"));
}

#[test]
fn env_hover_without_definition() {
    let out = format_env("UNKNOWN", None);
    assert!(out.contains("**env('UNKNOWN')**"));
    assert!(out.contains("*(not defined in .env)*"));
}

// ============================================================================
// Translation / middleware / binding
// ============================================================================

#[test]
fn translation_hover_strips_outer_quotes_in_block() {
    let out = format_translation(
        "validation.required",
        Some("'The :attribute field is required.'"),
        Some("lang/en/validation.php"),
    );
    assert!(out.contains("**__('validation.required')**"));
    // Outer quotes stripped from the block.
    assert!(out.contains("```\nThe :attribute field is required.\n```"));
    assert!(out.contains("at lang/en/validation.php"));
}

#[test]
fn translation_hover_with_vendor_source() {
    let out = format_translation(
        "filament-tables::table.actions.filter.label",
        Some("'Filter'"),
        Some("lang/vendor/filament-tables/en/table.php"),
    );
    assert!(out.contains("**__('filament-tables::table.actions.filter.label')**"));
    assert!(out.contains("at lang/vendor/filament-tables/en/table.php"));
}

#[test]
fn translation_hover_without_value() {
    let out = format_translation("validation.missing", None, None);
    assert!(out.contains("**__('validation.missing')**"));
    assert!(out.contains("*(translation not found for default locale)*"));
}

#[test]
fn middleware_hover_with_class_and_source() {
    let out = format_middleware(
        "auth",
        Some("App\\Http\\Middleware\\Authenticate"),
        Some("bootstrap/app.php"),
    );
    assert!(out.contains("**middleware('auth')**"));
    assert!(out.contains("```php"));
    assert!(out.contains("App\\Http\\Middleware\\Authenticate"));
    assert!(out.contains("at bootstrap/app.php"));
}

#[test]
fn middleware_hover_without_class() {
    let out = format_middleware("ghost", None, None);
    assert!(out.contains("*(alias not registered)*"));
}

#[test]
fn asset_hover_with_vite_helper_label() {
    // Vite::asset path. The helper label is what the developer typed at the
    // callsite, so the bold header reads like the actual PHP/Blade.
    let out = format_asset(
        "resources/js/app.js",
        "Vite::asset",
        Some("resources/js/app.js"),
    );
    assert!(out.contains("**Vite::asset('resources/js/app.js')**"));
    assert!(out.contains("at resources/js/app.js"));
}

#[test]
fn asset_hover_with_plain_asset_helper() {
    let out = format_asset("css/app.css", "asset", Some("public/css/app.css"));
    assert!(out.contains("**asset('css/app.css')**"));
    assert!(out.contains("at public/css/app.css"));
}

#[test]
fn asset_hover_without_resolved_path() {
    let out = format_asset("missing.css", "asset", None);
    assert!(out.contains("**asset('missing.css')**"));
    assert!(out.contains("*(file not found)*"));
}

#[test]
fn url_hover_with_resolved_path() {
    let out = format_url("/favicon.ico", Some("public/favicon.ico"));
    assert!(out.contains("**url('/favicon.ico')**"));
    assert!(out.contains("at public/favicon.ico"));
}

#[test]
fn url_hover_without_resolved_path() {
    let out = format_url("/missing", None);
    assert!(out.contains("**url('/missing')**"));
    assert!(out.contains("*(file not found)*"));
}

#[test]
fn binding_hover_with_class() {
    let out = format_binding(
        "cache",
        Some("Illuminate\\Cache\\Repository"),
        Some("app/Providers/AppServiceProvider.php"),
    );
    assert!(out.contains("**app('cache')**"));
    assert!(out.contains("Illuminate\\Cache\\Repository"));
    assert!(!out.contains("Concrete"));
}

// ============================================================================
// Blade variable — the intelephense-shape one
// ============================================================================

fn property_decl(
    text: &str,
    line: u32,
    description: Option<&str>,
    tags: &[&str],
) -> PropertyDeclaration {
    PropertyDeclaration {
        declaration_text: text.to_string(),
        line,
        description: description.map(|s| s.to_string()),
        phpdoc_tags: tags.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn blade_variable_property_with_class_and_declaration() {
    let decl = property_decl(
        "public string $email;",
        41,
        Some("The user's email address."),
        &[],
    );
    let out = format_blade_variable(&BladeVariableHover {
        var_name: "user",
        property: Some("email"),
        var_type: Some("App\\Models\\User"),
        declaration: Some(&decl),
        defined_in: Some("app/Models/User.php:42"),
    });
    assert!(
        out.contains("**App\\Models\\User::$email**"),
        "expected class-qualified header, got: {}",
        out
    );
    assert!(out.contains("The user's email address."));
    assert!(out.contains("```php\npublic string $email;\n```"));
    assert!(out.contains("at app/Models/User.php:42"));
}

#[test]
fn blade_variable_renders_phpdoc_tags() {
    let decl = property_decl(
        "public function authorize($ability, $arguments = [])",
        24,
        Some("Authorize a given action for the current user."),
        &[
            "@param mixed $ability",
            "@param mixed $arguments",
            "@return \\Illuminate\\Auth\\Access\\Response",
            "@throws \\Illuminate\\Auth\\Access\\AuthorizationException",
        ],
    );
    let out = format_blade_variable(&BladeVariableHover {
        var_name: "controller",
        property: Some("authorize"),
        var_type: Some("App\\Http\\Controllers\\Controller"),
        declaration: Some(&decl),
        defined_in: Some("app/Http/Controllers/Controller.php:25"),
    });
    assert!(out.contains("Authorize a given action for the current user."));
    assert!(out.contains("@param mixed $ability"));
    assert!(out.contains("@return \\Illuminate\\Auth\\Access\\Response"));
    assert!(out.contains("@throws \\Illuminate\\Auth\\Access\\AuthorizationException"));
}

#[test]
fn blade_variable_primitive_parent_uses_var_arrow_prop() {
    // Parent type is `mixed` (primitive). Don't render `mixed::$username` —
    // fall back to the `$user->username` shape.
    let out = format_blade_variable(&BladeVariableHover {
        var_name: "user",
        property: Some("username"),
        var_type: Some("mixed"),
        declaration: None,
        defined_in: None,
    });
    assert!(!out.contains("mixed::"), "got: {}", out);
    assert!(out.contains("$`user->username`"));
}

#[test]
fn blade_variable_root_with_class_type() {
    let out = format_blade_variable(&BladeVariableHover {
        var_name: "form",
        property: None,
        var_type: Some("App\\Livewire\\ContactForm"),
        declaration: None,
        defined_in: Some("app/Livewire/ContactForm.php"),
    });
    assert!(out.contains("**$form** : `App\\Livewire\\ContactForm`"));
    assert!(out.contains("at app/Livewire/ContactForm.php"));
}

#[test]
fn blade_variable_unresolved() {
    let out = format_blade_variable(&BladeVariableHover {
        var_name: "orphan",
        property: None,
        var_type: None,
        declaration: None,
        defined_in: None,
    });
    assert!(out.contains("**$orphan**"));
    assert!(
        !out.contains("\n\nat "),
        "no source line when no defined_in info"
    );
    assert!(!out.contains("Type:"));
}

#[test]
fn at_line_renders_passed_string_verbatim() {
    // The formatter renders `at <source_display>` verbatim — caller is
    // responsible for wrapping in backticks, markdown links, etc. This is
    // what enables the hover dispatcher (in main.rs) to pass full
    // `file://`-clickable links straight through.
    let link = "[`app/Models/User.php:42`](file:///Users/mike/project/app/Models/User.php#L42)";
    let out = format_view("users.profile", Some(link), None);
    assert!(
        out.contains(&format!("at {}", link)),
        "expected raw link, got: {}",
        out
    );
    // The link contains a `file://` URL so Zed renders it as clickable.
    assert!(out.contains("file:///"));
}

#[test]
fn is_class_like_type_distinguishes_classes_from_primitives() {
    assert!(is_class_like_type("App\\Models\\User"));
    assert!(is_class_like_type("\\App\\Models\\User"));
    assert!(is_class_like_type("Carbon"));
    assert!(is_class_like_type("Collection"));
    assert!(is_class_like_type("?Carbon"));

    assert!(!is_class_like_type("mixed"));
    assert!(!is_class_like_type("string"));
    assert!(!is_class_like_type("int"));
    assert!(!is_class_like_type("?int"));
    assert!(!is_class_like_type("null"));
    assert!(!is_class_like_type("array"));
}
