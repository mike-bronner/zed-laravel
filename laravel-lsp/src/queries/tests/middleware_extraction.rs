//! Integration tests for middleware extraction across all the shapes
//! Laravel uses to declare and register middleware.
//!
//! Covers:
//! - `Route::group(['middleware' => ...])` configuration arrays
//! - `$middlewareAliases` / `$middlewareGroups` properties (Kernel.php)
//! - Orchestra Testbench Kernel shape
//! - Laravel 11+ `defaultAliases()` / `getMiddlewareGroups()` methods
//! - `bootstrap/app.php` `$middleware->alias(...)` calls (single + array)
//! - `bootstrap/app.php` `$middleware->group(...)` calls
//! - Service-provider `$router->aliasMiddleware(...)` registration
//!
//! Relocated from the inline `mod tests` block in `src/queries.rs` so
//! business logic and test logic don't share a file.

use super::super::*;
use crate::parser::{language_php, parse_php};

#[test]
fn from_route_group() {
    // Test extracting middleware from Route::group() configuration arrays
    let php_code = r#"<?php
Route::group([
    'prefix' => 'api/v1',
    'middleware' => ['api', 'auth'],
], function () {});

Route::group([
    'middleware' => 'web',
], function () {});
"#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns =
        extract_all_php_patterns(&tree, php_code, &lang).expect("Should extract patterns");

    let middleware_names: Vec<&str> = patterns
        .middleware_calls
        .iter()
        .map(|m| m.middleware_name)
        .collect();

    assert!(
        middleware_names.contains(&"api"),
        "Should find 'api' middleware from array"
    );
    assert!(
        middleware_names.contains(&"auth"),
        "Should find 'auth' middleware from array"
    );
    assert!(
        middleware_names.contains(&"web"),
        "Should find 'web' middleware from string"
    );
}

#[test]
fn alias_definitions() {
    // Test extracting middleware alias definitions from Kernel.php style properties
    let php_code = r#"<?php
class Kernel {
    protected $middlewareAliases = [
        'auth' => Authenticate::class,
        'guest' => RedirectIfAuthenticated::class,
        'verified' => \Illuminate\Auth\Middleware\EnsureEmailIsVerified::class,
    ];

    protected $middlewareGroups = [
        'web' => [
            EncryptCookies::class,
            AddQueuedCookiesToResponse::class,
        ],
        'api' => [
            ThrottleRequests::class,
        ],
    ];
}
"#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns =
        extract_all_php_patterns(&tree, php_code, &lang).expect("Should extract patterns");

    // Check middleware alias definitions
    let alias_keys: Vec<&str> = patterns
        .middleware_alias_defs
        .iter()
        .map(|m| m.alias)
        .collect();
    let alias_classes: Vec<&str> = patterns
        .middleware_alias_defs
        .iter()
        .map(|m| m.class_name)
        .collect();

    assert!(alias_keys.contains(&"auth"), "Should find 'auth' alias");
    assert!(alias_keys.contains(&"guest"), "Should find 'guest' alias");
    assert!(
        alias_keys.contains(&"verified"),
        "Should find 'verified' alias"
    );
    assert!(
        alias_classes.contains(&"Authenticate"),
        "Should find Authenticate class"
    );
    assert!(
        alias_classes.contains(&"RedirectIfAuthenticated"),
        "Should find RedirectIfAuthenticated class"
    );

    // Check middleware group definitions
    let group_names: Vec<&str> = patterns
        .middleware_group_defs
        .iter()
        .map(|m| m.group_name)
        .collect();

    assert!(group_names.contains(&"web"), "Should find 'web' group");
    assert!(group_names.contains(&"api"), "Should find 'api' group");
}

#[test]
fn from_testbench_kernel() {
    // Test with Orchestra Testbench Kernel.php format.
    // This is the actual format used by testbench-core.
    let php_code = r#"<?php

namespace Orchestra\Testbench\Http;

use Illuminate\Foundation\Http\Kernel as HttpKernel;

class Kernel extends HttpKernel
{
    protected $middlewareGroups = [
        'web' => [
            \Illuminate\Cookie\Middleware\EncryptCookies::class,
            \Illuminate\Cookie\Middleware\AddQueuedCookiesToResponse::class,
            \Illuminate\Session\Middleware\StartSession::class,
            \Illuminate\View\Middleware\ShareErrorsFromSession::class,
            \Illuminate\Foundation\Http\Middleware\ValidateCsrfToken::class,
            \Illuminate\Routing\Middleware\SubstituteBindings::class,
        ],
        'api' => [
            \Illuminate\Routing\Middleware\ThrottleRequests::class.':api',
            \Illuminate\Routing\Middleware\SubstituteBindings::class,
        ],
    ];

    protected $middlewareAliases = [
        'auth' => \Illuminate\Auth\Middleware\Authenticate::class,
        'auth.basic' => \Illuminate\Auth\Middleware\AuthenticateWithBasicAuth::class,
        'auth.session' => \Illuminate\Session\Middleware\AuthenticateSession::class,
        'cache.headers' => \Illuminate\Http\Middleware\SetCacheHeaders::class,
        'can' => \Illuminate\Auth\Middleware\Authorize::class,
        'guest' => \Orchestra\Testbench\Http\Middleware\RedirectIfAuthenticated::class,
        'password.confirm' => \Illuminate\Auth\Middleware\RequirePassword::class,
        'precognitive' => \Illuminate\Foundation\Http\Middleware\HandlePrecognitiveRequests::class,
        'signed' => \Illuminate\Routing\Middleware\ValidateSignature::class,
        'throttle' => \Illuminate\Routing\Middleware\ThrottleRequests::class,
        'verified' => \Illuminate\Auth\Middleware\EnsureEmailIsVerified::class,
    ];
}
"#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns =
        extract_all_php_patterns(&tree, php_code, &lang).expect("Should extract patterns");

    // Check middleware group definitions
    let group_names: Vec<&str> = patterns
        .middleware_group_defs
        .iter()
        .map(|m| m.group_name)
        .collect();

    assert!(group_names.contains(&"web"), "Should find 'web' group");
    assert!(group_names.contains(&"api"), "Should find 'api' group");

    // Check middleware alias definitions
    let alias_keys: Vec<&str> = patterns
        .middleware_alias_defs
        .iter()
        .map(|m| m.alias)
        .collect();

    assert!(alias_keys.contains(&"auth"), "Should find 'auth' alias");
    assert!(alias_keys.contains(&"guest"), "Should find 'guest' alias");
    assert!(alias_keys.contains(&"can"), "Should find 'can' alias");
    assert!(
        alias_keys.contains(&"throttle"),
        "Should find 'throttle' alias"
    );
    assert!(
        alias_keys.contains(&"verified"),
        "Should find 'verified' alias"
    );
}

#[test]
fn from_laravel11_method_aliases() {
    // Test extracting middleware from Laravel 11+ defaultAliases() method body.
    // This pattern uses $aliases = [...] inside a method.
    let php_code = r#"<?php

namespace Illuminate\Foundation\Configuration;

class Middleware
{
    protected function defaultAliases()
    {
        $aliases = [
            'auth' => \Illuminate\Auth\Middleware\Authenticate::class,
            'auth.basic' => \Illuminate\Auth\Middleware\AuthenticateWithBasicAuth::class,
            'guest' => \Illuminate\Auth\Middleware\RedirectIfAuthenticated::class,
            'verified' => \Illuminate\Auth\Middleware\EnsureEmailIsVerified::class,
        ];

        return $aliases;
    }
}
"#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns =
        extract_all_php_patterns(&tree, php_code, &lang).expect("Should extract patterns");

    // Check middleware alias definitions from method body
    let alias_keys: Vec<&str> = patterns
        .middleware_alias_defs
        .iter()
        .map(|m| m.alias)
        .collect();

    assert!(
        alias_keys.contains(&"auth"),
        "Should find 'auth' alias from $aliases assignment"
    );
    assert!(
        alias_keys.contains(&"auth.basic"),
        "Should find 'auth.basic' alias"
    );
    assert!(alias_keys.contains(&"guest"), "Should find 'guest' alias");
    assert!(
        alias_keys.contains(&"verified"),
        "Should find 'verified' alias"
    );
}

#[test]
fn from_laravel11_method_groups() {
    // Test extracting middleware groups from Laravel 11+ getMiddlewareGroups() method.
    let php_code = r#"<?php

namespace Illuminate\Foundation\Configuration;

class Middleware
{
    public function getMiddlewareGroups()
    {
        $middleware = [
            'web' => [
                \Illuminate\Cookie\Middleware\EncryptCookies::class,
            ],
            'api' => [
                \Illuminate\Routing\Middleware\ThrottleRequests::class,
            ],
        ];

        return $middleware;
    }
}
"#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns =
        extract_all_php_patterns(&tree, php_code, &lang).expect("Should extract patterns");

    // Check middleware group definitions from method body
    let group_names: Vec<&str> = patterns
        .middleware_group_defs
        .iter()
        .map(|m| m.group_name)
        .collect();

    assert!(
        group_names.contains(&"web"),
        "Should find 'web' group from $middleware assignment"
    );
    assert!(
        group_names.contains(&"api"),
        "Should find 'api' group from $middleware assignment"
    );
}

#[test]
fn from_bootstrap_app_alias_call() {
    // Test extracting middleware from $middleware->alias() calls in bootstrap/app.php
    let php_code = r#"<?php

use App\Http\Middleware\CustomAuth;
use App\Http\Middleware\ApiRateLimiter;

return Application::configure(basePath: dirname(__DIR__))
    ->withMiddleware(function (Middleware $middleware) {
        $middleware->alias('custom.auth', CustomAuth::class);
        $middleware->alias('api.rate', ApiRateLimiter::class);
    });
"#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns =
        extract_all_php_patterns(&tree, php_code, &lang).expect("Should extract patterns");

    // Check middleware alias definitions from ->alias() calls
    let alias_keys: Vec<&str> = patterns
        .middleware_alias_defs
        .iter()
        .map(|m| m.alias)
        .collect();

    assert!(
        alias_keys.contains(&"custom.auth"),
        "Should find 'custom.auth' alias from ->alias() call"
    );
    assert!(
        alias_keys.contains(&"api.rate"),
        "Should find 'api.rate' alias from ->alias() call"
    );
}

#[test]
fn from_bootstrap_app_alias_array() {
    // Test extracting middleware from $middleware->alias([...]) with array argument
    let php_code = r#"<?php

return Application::configure(basePath: dirname(__DIR__))
    ->withMiddleware(function (Middleware $middleware) {
        $middleware->alias([
            'custom.auth' => CustomAuth::class,
            'custom.guest' => CustomGuest::class,
        ]);
    });
"#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns =
        extract_all_php_patterns(&tree, php_code, &lang).expect("Should extract patterns");

    // Check middleware alias definitions from ->alias([...]) call
    let alias_keys: Vec<&str> = patterns
        .middleware_alias_defs
        .iter()
        .map(|m| m.alias)
        .collect();

    assert!(
        alias_keys.contains(&"custom.auth"),
        "Should find 'custom.auth' alias from ->alias([...]) call"
    );
    assert!(
        alias_keys.contains(&"custom.guest"),
        "Should find 'custom.guest' alias from ->alias([...]) call"
    );
}

#[test]
fn from_bootstrap_app_group_call() {
    // Test extracting middleware from $middleware->group() calls in bootstrap/app.php
    let php_code = r#"<?php

return Application::configure(basePath: dirname(__DIR__))
    ->withMiddleware(function (Middleware $middleware) {
        $middleware->group('custom', [
            FirstMiddleware::class,
            SecondMiddleware::class,
        ]);
    });
"#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns =
        extract_all_php_patterns(&tree, php_code, &lang).expect("Should extract patterns");

    // Check middleware group definitions from ->group() call
    let group_names: Vec<&str> = patterns
        .middleware_group_defs
        .iter()
        .map(|m| m.group_name)
        .collect();

    assert!(
        group_names.contains(&"custom"),
        "Should find 'custom' group from ->group() call"
    );
}

#[test]
fn from_service_provider_router() {
    // Test extracting middleware from $router->aliasMiddleware() in service providers
    let php_code = r#"<?php

namespace App\Providers;

class RouteServiceProvider extends ServiceProvider
{
    public function boot()
    {
        $router = $this->app->make('router');
        $router->aliasMiddleware('custom', CustomMiddleware::class);
        $router->aliasMiddleware('another', AnotherMiddleware::class);
    }
}
"#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns =
        extract_all_php_patterns(&tree, php_code, &lang).expect("Should extract patterns");

    // Check middleware alias definitions from $router->aliasMiddleware() calls
    let alias_keys: Vec<&str> = patterns
        .middleware_alias_defs
        .iter()
        .map(|m| m.alias)
        .collect();

    assert!(
        alias_keys.contains(&"custom"),
        "Should find 'custom' alias from $router->aliasMiddleware()"
    );
    assert!(
        alias_keys.contains(&"another"),
        "Should find 'another' alias from $router->aliasMiddleware()"
    );
}
