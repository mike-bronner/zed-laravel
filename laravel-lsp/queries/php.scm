; Tree-sitter query for detecting Laravel patterns in PHP files
;
; This file uses tree-sitter query syntax (S-expressions)
; to match patterns in the PHP Abstract Syntax Tree (AST)
;
; Query syntax:
;   (node_type) - matches a node of this type
;   field_name: - matches a named field on the node
;   @capture_name - captures the matched node for later use
;   (#eq? @var "value") - predicate to filter matches
;
; Reference: https://tree-sitter.github.io/tree-sitter/using-parsers#pattern-matching-with-queries

; ============================================================================
; Pattern 1: view('view.name') function calls
; ============================================================================
; Matches: view('users.profile')
;          view("admin.dashboard")
;
; AST structure for function calls:
;   function_call_expression
;     function: (name or qualified_name)
;     arguments: (arguments ...)

; Single-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    (argument
      (string
        (string_content) @view_name)))
  (#eq? @function_name "view"))

; Double-quoted strings (encapsed_string)
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    (argument
      (encapsed_string
        (string_content) @view_name)))
  (#eq? @function_name "view"))

; ============================================================================
; Pattern 2: View::make('view.name') static method calls
; ============================================================================
; Matches: View::make('users.profile')
;          \View::make('admin.dashboard')
;
; AST structure for static method calls:
;   scoped_call_expression
;     scope: (name)
;     name: (name)
;     arguments: (arguments ...)

; Single-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (string
        (string_content) @view_name)))
  (#eq? @class_name "View")
  (#eq? @method_name "make"))

; Double-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (encapsed_string
        (string_content) @view_name)))
  (#eq? @class_name "View")
  (#eq? @method_name "make"))

; Also match fully qualified View class - single quotes
(scoped_call_expression
  scope: (qualified_name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (string
        (string_content) @view_name)))
  (#match? @class_name ".*View$")
  (#eq? @method_name "make"))

; Also match fully qualified View class - double quotes
(scoped_call_expression
  scope: (qualified_name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (encapsed_string
        (string_content) @view_name)))
  (#match? @class_name ".*View$")
  (#eq? @method_name "make"))

; ============================================================================
; Pattern 3: Route::view('/path', 'view.name') - Route view registration
; ============================================================================
; Matches: Route::view('/home', 'welcome')
;          Route::view('/about', 'pages.about')
;
; AST structure for static method calls:
;   scoped_call_expression
;     scope: (name) - "Route"
;     name: (name) - "view"
;     arguments: (arguments ...)
;
; IMPORTANT: We capture the SECOND argument (the view name), not the first (route path)

; Single-quoted view name (second argument)
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument)
    (argument
      (string
        (string_content) @route_view_name)))
  (#eq? @class_name "Route")
  (#eq? @method_name "view"))

; Double-quoted view name (second argument)
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument)
    (argument
      (encapsed_string
        (string_content) @route_view_name)))
  (#eq? @class_name "Route")
  (#eq? @method_name "view"))

; ============================================================================
; Pattern 4: Volt::route('/path', 'view.name') - Volt route registration
; ============================================================================
; Matches: Volt::route('/home', 'welcome')
;          Volt::route('/about', 'pages.about')
;
; Same as Route::view() - captures the SECOND argument (view name)

; Single-quoted view name (second argument)
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument)
    (argument
      (string
        (string_content) @route_view_name)))
  (#eq? @class_name "Volt")
  (#eq? @method_name "route"))

; Double-quoted view name (second argument)
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument)
    (argument
      (encapsed_string
        (string_content) @route_view_name)))
  (#eq? @class_name "Volt")
  (#eq? @method_name "route"))

; ============================================================================
; Pattern 5: env('VAR_NAME') or env('VAR_NAME', 'default') function calls
; ============================================================================
; Matches: env('APP_NAME', 'Laravel')
;          env("DB_HOST")
;
; This pattern captures the FIRST argument to env() which is the variable name

; Single-quoted strings - only match first argument
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @env_var)))
  (#eq? @function_name "env"))

; Double-quoted strings (encapsed_string) - only match first argument
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @env_var)))
  (#eq? @function_name "env"))

; ============================================================================
; Pattern 5b: Env::get('VAR_NAME') - Env facade
; ============================================================================
; Matches: Env::get('APP_NAME')
;          Env::get("DB_HOST", 'localhost')
;          \Env::get('APP_KEY')
;
; The Env facade is the OO equivalent of the env() helper. Uses the same
; @env_var capture name so existing EnvMatch dispatch handles it.

; Single-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @env_var)))
  (#eq? @class_name "Env")
  (#eq? @method_name "get"))

; Double-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @env_var)))
  (#eq? @class_name "Env")
  (#eq? @method_name "get"))

; Fully qualified Env class - single quotes
(scoped_call_expression
  scope: (qualified_name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @env_var)))
  (#match? @class_name ".*Env$")
  (#eq? @method_name "get"))

; Fully qualified Env class - double quotes
(scoped_call_expression
  scope: (qualified_name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @env_var)))
  (#match? @class_name ".*Env$")
  (#eq? @method_name "get"))

; ============================================================================
; Pattern 6: config('config.key') function calls
; ============================================================================
; Matches: config('app.name')
;          config("database.connections.mysql.host")
;
; This pattern captures config key access in application code

; Single-quoted strings - only match first argument
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @config_key)))
  (#eq? @function_name "config"))

; Double-quoted strings (encapsed_string) - only match first argument
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @config_key)))
  (#eq? @function_name "config"))

; ============================================================================
; Pattern 6b: Config::get('key'), Config::string('key'), etc. - Facade methods
; ============================================================================
; Matches: Config::get('app.name')
;          Config::string('app.name')
;          Config::integer('app.timeout') / Config::int('app.timeout')
;          Config::boolean('app.debug')   / Config::bool('app.debug')
;          Config::float('app.weight')
;          Config::array('app.providers')
;
; This captures config key access via the Config facade. Both legacy
; (integer/boolean) and modern (int/bool/float) accessor names are matched.

; Single-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @config_key)))
  (#eq? @class_name "Config")
  (#match? @method_name "^(get|string|int|integer|bool|boolean|float|array|set|has)$"))

; Double-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @config_key)))
  (#eq? @class_name "Config")
  (#match? @method_name "^(get|string|int|integer|bool|boolean|float|array|set|has)$"))

; Also match fully qualified Config class - single quotes
(scoped_call_expression
  scope: (qualified_name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @config_key)))
  (#match? @class_name ".*Config$")
  (#match? @method_name "^(get|string|int|integer|bool|boolean|float|array|set|has)$"))

; Also match fully qualified Config class - double quotes
(scoped_call_expression
  scope: (qualified_name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @config_key)))
  (#match? @class_name ".*Config$")
  (#match? @method_name "^(get|string|int|integer|bool|boolean|float|array|set|has)$"))

; ============================================================================
; Pattern 6c: Config::getMany(['key1', 'key2']) - Bulk config retrieval
; ============================================================================
; Matches: Config::getMany(['app.name', 'app.env'])
;          Config::getMany(["database.default", "database.connections.mysql.host"])
;
; Each array element is captured as a separate config key, the same way
; middleware arrays in Route::middleware([...]) are captured.

; Single-quoted strings in array
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (string
            (string_content) @config_key)))))
  (#eq? @class_name "Config")
  (#eq? @method_name "getMany"))

; Double-quoted strings in array
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (encapsed_string
            (string_content) @config_key)))))
  (#eq? @class_name "Config")
  (#eq? @method_name "getMany"))

; ============================================================================
; Pattern 6d: config()->string('key') - Fluent instance accessors
; ============================================================================
; Matches: config()->string('app.name')
;          config()->int('app.timeout')
;          config()->bool('app.debug')
;          config()->float('app.weight')
;          config()->array('app.providers')
;          config()->integer(...)   (legacy alias)
;          config()->boolean(...)   (legacy alias)
;          config()->get('app.name')
;
; The argumentless config() helper returns the Repository instance, which
; exposes typed accessors. The AST is a member_call_expression whose object
; is the function_call_expression for config().

; Single-quoted strings
(member_call_expression
  object: (function_call_expression
    function: (name) @config_fn)
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @config_key)))
  (#eq? @config_fn "config")
  (#match? @method_name "^(get|string|int|integer|bool|boolean|float|array|has)$"))

; Double-quoted strings
(member_call_expression
  object: (function_call_expression
    function: (name) @config_fn)
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @config_key)))
  (#eq? @config_fn "config")
  (#match? @method_name "^(get|string|int|integer|bool|boolean|float|array|has)$"))

; ============================================================================
; Pattern 7: route('route.name') function calls
; ============================================================================
; Matches: route('home')
;          route('admin.dashboard')
;          route("user.profile", ['id' => 1])
;
; Captures route name for navigation to route definition

; Single-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @route_name)))
  (#eq? @function_name "route"))

; Double-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @route_name)))
  (#eq? @function_name "route"))

; ============================================================================
; Pattern 7b: signed_route('route.name') function calls
; ============================================================================
; Matches: signed_route('verify.email')
;          signed_route("password.reset", ['token' => $token])
;
; Same resolution as route() — looks up a named route. Captures route name.

; Single-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @route_name)))
  (#eq? @function_name "signed_route"))

; Double-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @route_name)))
  (#eq? @function_name "signed_route"))

; ============================================================================
; Pattern 8: Route::middleware('auth') - Static method calls with single middleware
; ============================================================================
; Matches: Route::middleware('auth')
;          Route::withoutMiddleware('verified')
;
; AST structure for static method calls:
;   scoped_call_expression
;     scope: (name) - "Route"
;     name: (name) - "middleware" or "withoutMiddleware"
;     arguments: (arguments ...)

; Single-quoted string middleware
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (string
        (string_content) @middleware_name)))
  (#eq? @class_name "Route")
  (#match? @method_name "^(middleware|withoutMiddleware)$"))

; Double-quoted string middleware
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (encapsed_string
        (string_content) @middleware_name)))
  (#eq? @class_name "Route")
  (#match? @method_name "^(middleware|withoutMiddleware)$"))

; ============================================================================
; Pattern 9: Route::middleware(['auth', 'web']) - Array of middleware
; ============================================================================
; Matches: Route::middleware(['auth', 'verified'])
;          Route::withoutMiddleware(['guest'])
;
; This captures individual middleware strings within an array argument

; Single-quoted strings in array
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (string
            (string_content) @middleware_name)))))
  (#eq? @class_name "Route")
  (#match? @method_name "^(middleware|withoutMiddleware)$"))

; Double-quoted strings in array
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (encapsed_string
            (string_content) @middleware_name)))))
  (#eq? @class_name "Route")
  (#match? @method_name "^(middleware|withoutMiddleware)$"))

; ============================================================================
; Pattern 10: ->middleware('auth') - Chained method calls with single middleware
; ============================================================================
; Matches: Route::get('/dashboard')->middleware('auth')
;          $route->middleware('verified')
;          ->withoutMiddleware('guest')
;
; AST structure for member call expressions:
;   member_call_expression
;     name: (name) - "middleware" or "withoutMiddleware"
;     arguments: (arguments ...)

; Single-quoted string middleware in chained calls
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    (argument
      (string
        (string_content) @middleware_name)))
  (#match? @method_name "^(middleware|withoutMiddleware)$"))

; Double-quoted string middleware in chained calls
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    (argument
      (encapsed_string
        (string_content) @middleware_name)))
  (#match? @method_name "^(middleware|withoutMiddleware)$"))

; ============================================================================
; Pattern 11: ->middleware(['auth', 'web']) - Chained method calls with array
; ============================================================================
; Matches: Route::get('/admin')->middleware(['auth', 'verified'])
;          $route->withoutMiddleware(['guest'])

; Single-quoted strings in array (chained)
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (string
            (string_content) @middleware_name)))))
  (#match? @method_name "^(middleware|withoutMiddleware)$"))

; Double-quoted strings in array (chained)
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (encapsed_string
            (string_content) @middleware_name)))))
  (#match? @method_name "^(middleware|withoutMiddleware)$"))

; ============================================================================
; Pattern 11b: 'middleware' => [...] in Route::group() configuration arrays
; ============================================================================
; Matches: Route::group(['middleware' => ['api', 'auth']], ...)
;          Route::group(['middleware' => 'web'], ...)
;          ['middleware' => ['auth']] in any route configuration context
;
; This captures middleware specified in configuration arrays, not method calls

; Single-quoted middleware in array: 'middleware' => ['api']
(array_element_initializer
  (string
    (string_content) @_mw_key)
  (array_creation_expression
    (array_element_initializer
      (string
        (string_content) @middleware_name)))
  (#eq? @_mw_key "middleware"))

; Double-quoted middleware in array: 'middleware' => ["api"]
(array_element_initializer
  (string
    (string_content) @_mw_key)
  (array_creation_expression
    (array_element_initializer
      (encapsed_string
        (string_content) @middleware_name)))
  (#eq? @_mw_key "middleware"))

; Single string middleware: 'middleware' => 'web'
(array_element_initializer
  (string
    (string_content) @_mw_key)
  (string
    (string_content) @middleware_name)
  (#eq? @_mw_key "middleware"))

; Double-quoted single string middleware: 'middleware' => "web"
(array_element_initializer
  (string
    (string_content) @_mw_key)
  (encapsed_string
    (string_content) @middleware_name)
  (#eq? @_mw_key "middleware"))

; ============================================================================
; Pattern 12: __('translation.key') - Translation helper function
; ============================================================================
; Matches: __('messages.welcome')
;          __("auth.failed")
;
; This is the most common translation helper in Laravel

; Single-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @translation_key)))
  (#eq? @function_name "__"))

; Double-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @translation_key)))
  (#eq? @function_name "__"))

; ============================================================================
; Pattern 13: trans('translation.key') - Trans helper function
; ============================================================================
; Matches: trans('messages.welcome')
;          trans("validation.required")

; Single-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @translation_key)))
  (#eq? @function_name "trans"))

; Double-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @translation_key)))
  (#eq? @function_name "trans"))

; ============================================================================
; Pattern 14: trans_choice('translation.key', $count) - Pluralization helper
; ============================================================================
; Matches: trans_choice('messages.apples', 10)
;          trans_choice("messages.minutes_ago", $minutes)

; Single-quoted strings - only match first argument (the translation key)
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @translation_key)))
  (#eq? @function_name "trans_choice"))

; Double-quoted strings - only match first argument
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @translation_key)))
  (#eq? @function_name "trans_choice"))

; ============================================================================
; Pattern 15: Lang::get('translation.key') - Facade method
; ============================================================================
; Matches: Lang::get('messages.welcome')
;          Lang::has('messages.welcome')
;          Lang::hasForLocale('messages.welcome', 'es')
;          Lang::choice('messages.apples', 10)
;          \Lang::get("validation.email")

; Single-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @translation_key)))
  (#eq? @class_name "Lang")
  (#match? @method_name "^(get|has|hasForLocale|choice)$"))

; Double-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @translation_key)))
  (#eq? @class_name "Lang")
  (#match? @method_name "^(get|has|hasForLocale|choice)$"))

; Also match fully qualified Lang class - single quotes
(scoped_call_expression
  scope: (qualified_name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @translation_key)))
  (#match? @class_name ".*Lang$")
  (#match? @method_name "^(get|has|hasForLocale|choice)$"))

; Also match fully qualified Lang class - double quotes
(scoped_call_expression
  scope: (qualified_name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @translation_key)))
  (#match? @class_name ".*Lang$")
  (#match? @method_name "^(get|has|hasForLocale|choice)$"))

; ============================================================================
; Pattern 16: app('binding') / resolve('binding') - Container binding resolution
; ============================================================================
; Matches: app('auth'), resolve('auth')
;          app("cache"), resolve("cache")
;          app('App\Contracts\SomeInterface')
;
; This pattern captures container binding resolution using string identifiers

; app() - Single-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @binding_name)))
  (#eq? @function_name "app"))

; app() - Double-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @binding_name)))
  (#eq? @function_name "app"))

; resolve() - Single-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @binding_name)))
  (#eq? @function_name "resolve"))

; resolve() - Double-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @binding_name)))
  (#eq? @function_name "resolve"))

; ============================================================================
; Pattern 17: app(SomeClass::class) / resolve(Class::class) - Container binding with class reference
; ============================================================================
; Matches: app(UserService::class), resolve(UserService::class)
;          app(\App\Services\PaymentService::class)
;
; This pattern captures container resolution using ::class constants

; app() - Class name with ::class
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (class_constant_access_expression
        (name) @binding_class_name
        (name) @constant_name)))
  (#eq? @function_name "app")
  (#eq? @constant_name "class"))

; app() - Qualified class name with ::class
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (class_constant_access_expression
        (qualified_name) @binding_class_name
        (name) @constant_name)))
  (#eq? @function_name "app")
  (#eq? @constant_name "class"))

; resolve() - Class name with ::class
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (class_constant_access_expression
        (name) @binding_class_name
        (name) @constant_name)))
  (#eq? @function_name "resolve")
  (#eq? @constant_name "class"))

; resolve() - Qualified class name with ::class
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (class_constant_access_expression
        (qualified_name) @binding_class_name
        (name) @constant_name)))
  (#eq? @function_name "resolve")
  (#eq? @constant_name "class"))

; ============================================================================
; Pattern 17b: App::bound('key') / App::isShared('key') - App facade lookups
; ============================================================================
; Matches: App::bound('cache')
;          App::isShared('App\Contracts\SomeInterface')
;          \App::bound("auth")
;
; These are the OO facade equivalents of the app() / resolve() helpers,
; used to introspect the container by string binding name. Reuses the
; @binding_name capture so the existing BindingMatch dispatch handles it.

; Single-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @binding_name)))
  (#eq? @class_name "App")
  (#match? @method_name "^(bound|isShared)$"))

; Double-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @binding_name)))
  (#eq? @class_name "App")
  (#match? @method_name "^(bound|isShared)$"))

; Fully qualified App class - single quotes
(scoped_call_expression
  scope: (qualified_name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @binding_name)))
  (#match? @class_name ".*App$")
  (#match? @method_name "^(bound|isShared)$"))

; Fully qualified App class - double quotes
(scoped_call_expression
  scope: (qualified_name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @binding_name)))
  (#match? @class_name ".*App$")
  (#match? @method_name "^(bound|isShared)$"))

; ============================================================================
; Pattern 12b: Route::group(['middleware' => 'auth'], ...) - Group with middleware in options array
; ============================================================================
; Matches: Route::group(['middleware' => 'auth'], function() {...})
;          Route::group(['middleware' => ['auth', 'verified']], function() {...})
;
; This captures middleware specified in the options array of Route::group()

; Single middleware string in options array - single quotes key, single quotes value
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (string
            (string_content) @_key)
          (string
            (string_content) @middleware_name)))))
  (#eq? @class_name "Route")
  (#eq? @method_name "group")
  (#eq? @_key "middleware"))

; Single middleware string in options array - single quotes key, double quotes value
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (string
            (string_content) @_key)
          (encapsed_string
            (string_content) @middleware_name)))))
  (#eq? @class_name "Route")
  (#eq? @method_name "group")
  (#eq? @_key "middleware"))

; Array of middleware in options array - single quotes in nested array
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (string
            (string_content) @_key)
          (array_creation_expression
            (array_element_initializer
              (string
                (string_content) @middleware_name)))))))
  (#eq? @class_name "Route")
  (#eq? @method_name "group")
  (#eq? @_key "middleware"))

; Array of middleware in options array - double quotes in nested array
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (string
            (string_content) @_key)
          (array_creation_expression
            (array_element_initializer
              (encapsed_string
                (string_content) @middleware_name)))))))
  (#eq? @class_name "Route")
  (#eq? @method_name "group")
  (#eq? @_key "middleware"))

; ============================================================================
; Pattern 13: Asset and Path Helpers
; ============================================================================
; Matches: asset('images/logo.png')
;          public_path('index.php')
;          base_path('composer.json')
;          app_path('Models/User.php')
;          storage_path('logs/laravel.log')
;          database_path('seeders/UserSeeder.php')
;          lang_path('en/messages.php')
;          config_path('app.php')
;          resource_path('views/welcome.blade.php')
;          mix('css/app.css')
;          Vite::asset('resources/images/logo.svg')

; asset() - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @asset_path)))
  (#eq? @function_name "asset"))

; asset() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @asset_path)))
  (#eq? @function_name "asset"))

; public_path() - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @public_path)))
  (#eq? @function_name "public_path"))

; public_path() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @public_path)))
  (#eq? @function_name "public_path"))

; base_path() - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @base_path)))
  (#eq? @function_name "base_path"))

; base_path() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @base_path)))
  (#eq? @function_name "base_path"))

; app_path() - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @app_path)))
  (#eq? @function_name "app_path"))

; app_path() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @app_path)))
  (#eq? @function_name "app_path"))

; storage_path() - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @storage_path)))
  (#eq? @function_name "storage_path"))

; storage_path() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @storage_path)))
  (#eq? @function_name "storage_path"))

; database_path() - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @database_path)))
  (#eq? @function_name "database_path"))

; database_path() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @database_path)))
  (#eq? @function_name "database_path"))

; lang_path() - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @lang_path)))
  (#eq? @function_name "lang_path"))

; lang_path() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @lang_path)))
  (#eq? @function_name "lang_path"))

; config_path() - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @config_path)))
  (#eq? @function_name "config_path"))

; config_path() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @config_path)))
  (#eq? @function_name "config_path"))

; resource_path() - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @resource_path)))
  (#eq? @function_name "resource_path"))

; resource_path() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @resource_path)))
  (#eq? @function_name "resource_path"))

; mix() - single quotes (legacy Laravel Mix)
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @mix_path)))
  (#eq? @function_name "mix"))

; mix() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @mix_path)))
  (#eq? @function_name "mix"))

; Vite::asset() - single quotes
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @vite_asset_path)))
  (#eq? @class_name "Vite")
  (#eq? @method_name "asset"))

; Vite::asset() - double quotes
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @vite_asset_path)))
  (#eq? @class_name "Vite")
  (#eq? @method_name "asset"))

; ============================================================================
; Pattern 18: url('path') - URL helper function
; ============================================================================
; Matches: url('home')
;          url('/admin/dashboard')
;          url("api/users")
;
; Captures URL path for navigation to public files or route definitions

; Single-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @url_path)))
  (#eq? @function_name "url"))

; Double-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @url_path)))
  (#eq? @function_name "url"))

; ============================================================================
; Pattern 19: action('Controller@method') - Controller action URLs
; ============================================================================
; Matches: action('UserController@show')
;          action('App\Http\Controllers\AdminController@index')
;          action([UserController::class, 'show'])
;
; Captures controller action for navigation

; String syntax - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @action_name)))
  (#eq? @function_name "action"))

; String syntax - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @action_name)))
  (#eq? @function_name "action"))

; ============================================================================
; Pattern 20: redirect()->route('name') - Redirect to named route
; ============================================================================
; Matches: redirect()->route('home')
;          redirect()->route('user.profile', ['id' => 1])
;
; This captures the route name from redirect chains

; Single-quoted strings
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @route_name)))
  (#eq? @method_name "route"))

; Double-quoted strings (already captures route_name like the function)
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @route_name)))
  (#eq? @method_name "route"))

; ============================================================================
; Pattern 21: to_route('name') - Laravel 9+ redirect helper
; ============================================================================
; Matches: to_route('home')
;          to_route('user.profile', ['id' => 1])
;
; This is a shorthand for redirect()->route()

; Single-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @route_name)))
  (#eq? @function_name "to_route"))

; Double-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @route_name)))
  (#eq? @function_name "to_route"))

; ============================================================================
; Pattern 22: Route::has('name') - Check if named route exists
; ============================================================================
; Matches: Route::has('admin.dashboard')
;          Route::has('user.profile')
;
; Used to check if a named route exists before generating URLs

; Single-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @route_name)))
  (#eq? @class_name "Route")
  (#eq? @method_name "has"))

; Double-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @route_name)))
  (#eq? @class_name "Route")
  (#eq? @method_name "has"))

; ============================================================================
; Pattern 23: URL::route('name') / URL::signedRoute('name') - Generate URL
; ============================================================================
; Matches: URL::route('home')
;          URL::route('user.profile', ['id' => 1])
;          URL::signedRoute('verify.email')
;
; Alternative to route() / signed_route() helpers for generating URLs

; Single-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @route_name)))
  (#eq? @class_name "URL")
  (#match? @method_name "^(route|signedRoute)$"))

; Double-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @route_name)))
  (#eq? @class_name "URL")
  (#match? @method_name "^(route|signedRoute)$"))

; ============================================================================
; Pattern 24: Route::is('name') / Route::currentRouteNamed('name')
; ============================================================================
; Matches: Route::is('admin.*')
;          Route::is('user.profile')
;          Route::currentRouteNamed('dashboard')
;
; Used to check if the current route matches a pattern

; Single-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @route_name)))
  (#eq? @class_name "Route")
  (#match? @method_name "^(is|currentRouteNamed)$"))

; Double-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @route_name)))
  (#eq? @class_name "Route")
  (#match? @method_name "^(is|currentRouteNamed)$"))

; ============================================================================
; Pattern 25: $request->routeIs('name') - Request route checking
; ============================================================================
; Matches: $request->routeIs('profile')
;          $request->routeIs('admin.*')
;
; Check if the current request matches a route name pattern

; Single-quoted strings
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @route_name)))
  (#eq? @method_name "routeIs"))

; Double-quoted strings
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @route_name)))
  (#eq? @method_name "routeIs"))

; ============================================================================
; Pattern 26: $request->route()->named('name') - Check if route matches name
; ============================================================================
; Matches: $request->route()->named('profile')
;          $request->route()->named('admin.dashboard')
;
; Check if the current route has a specific name

; Single-quoted strings
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @route_name)))
  (#eq? @method_name "named"))

; Double-quoted strings
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @route_name)))
  (#eq? @method_name "named"))

; ============================================================================
; Pattern 27: Feature::active('feature-name') - Laravel Pennant feature flags
; ============================================================================
; Matches: Feature::active('new-api')
;          Feature::inactive('new-api')
;          Feature::value('purchase-button')
;          Feature::when('new-api', fn() => ...)
;          Feature::forget('new-api')
;          Feature::purge('new-api')
;
; Pennant provides feature flag functionality for Laravel applications

; Single-quoted strings - simple method calls
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @feature_method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @feature_name)))
  (#eq? @class_name "Feature")
  (#match? @feature_method_name "^(active|inactive|value|when|forget|purge)$"))

; Double-quoted strings - simple method calls
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @feature_method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @feature_name)))
  (#eq? @class_name "Feature")
  (#match? @feature_method_name "^(active|inactive|value|when|forget|purge)$"))

; Also match fully qualified Feature class - single quotes
(scoped_call_expression
  scope: (qualified_name) @class_name
  name: (name) @feature_method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @feature_name)))
  (#match? @class_name ".*Feature$")
  (#match? @feature_method_name "^(active|inactive|value|when|forget|purge)$"))

; Also match fully qualified Feature class - double quotes
(scoped_call_expression
  scope: (qualified_name) @class_name
  name: (name) @feature_method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @feature_name)))
  (#match? @class_name ".*Feature$")
  (#match? @feature_method_name "^(active|inactive|value|when|forget|purge)$"))

; ============================================================================
; Pattern 28: Feature::for($user)->active('feature-name') - Scoped feature checks
; ============================================================================
; Matches: Feature::for($user)->active('new-api')
;          Feature::for($team)->inactive('new-api')
;          Feature::for($user)->value('purchase-button')
;
; Pennant allows checking features for specific scopes (users, teams, etc.)

; Single-quoted strings - chained after for()
(member_call_expression
  object: (scoped_call_expression
    scope: (name) @class_name
    name: (name) @for_method
    (#eq? @class_name "Feature")
    (#eq? @for_method "for"))
  name: (name) @feature_method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @feature_name)))
  (#match? @feature_method_name "^(active|inactive|value|when)$"))

; Double-quoted strings - chained after for()
(member_call_expression
  object: (scoped_call_expression
    scope: (name) @class_name
    name: (name) @for_method
    (#eq? @class_name "Feature")
    (#eq? @for_method "for"))
  name: (name) @feature_method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @feature_name)))
  (#match? @feature_method_name "^(active|inactive|value|when)$"))

; ============================================================================
; Pattern 29: Feature::allAreActive(['feature-a', 'feature-b']) - Multiple features
; ============================================================================
; Matches: Feature::allAreActive(['new-api', 'beta-mode'])
;          Feature::someAreActive(['feature-a', 'feature-b'])
;          Feature::allAreInactive(['old-api', 'legacy'])
;          Feature::someAreInactive(['old-api', 'legacy'])
;
; Check multiple features at once

; Single-quoted strings in array - allAreActive/someAreActive
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @feature_method_name
  arguments: (arguments
    .
    (argument
      (array_creation_expression
        (array_element_initializer
          (string
            (string_content) @feature_name)))))
  (#eq? @class_name "Feature")
  (#match? @feature_method_name "^(allAreActive|someAreActive|allAreInactive|someAreInactive)$"))

; Double-quoted strings in array - allAreActive/someAreActive
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @feature_method_name
  arguments: (arguments
    .
    (argument
      (array_creation_expression
        (array_element_initializer
          (encapsed_string
            (string_content) @feature_name)))))
  (#eq? @class_name "Feature")
  (#match? @feature_method_name "^(allAreActive|someAreActive|allAreInactive|someAreInactive)$"))

; ============================================================================
; Pattern 30: Feature::active(NewApi::class) - Class-based features
; ============================================================================
; Matches: Feature::active(NewApi::class)
;          Feature::for($user)->active(NewApi::class)
;          Feature::inactive(\App\Features\BetaMode::class)
;
; Pennant supports class-based features where the class name is the feature

; Class constant with simple name
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @feature_method_name
  arguments: (arguments
    .
    (argument
      (class_constant_access_expression
        (name) @feature_class_name
        (name) @constant_name)))
  (#eq? @class_name "Feature")
  (#match? @feature_method_name "^(active|inactive|value|when|forget|purge)$")
  (#eq? @constant_name "class"))

; Class constant with qualified name
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @feature_method_name
  arguments: (arguments
    .
    (argument
      (class_constant_access_expression
        (qualified_name) @feature_class_name
        (name) @constant_name)))
  (#eq? @class_name "Feature")
  (#match? @feature_method_name "^(active|inactive|value|when|forget|purge)$")
  (#eq? @constant_name "class"))

; Class constant chained after for() - simple name
(member_call_expression
  object: (scoped_call_expression
    scope: (name) @class_name
    name: (name) @for_method
    (#eq? @class_name "Feature")
    (#eq? @for_method "for"))
  name: (name) @feature_method_name
  arguments: (arguments
    .
    (argument
      (class_constant_access_expression
        (name) @feature_class_name
        (name) @constant_name)))
  (#match? @feature_method_name "^(active|inactive|value|when)$")
  (#eq? @constant_name "class"))

; Class constant chained after for() - qualified name
(member_call_expression
  object: (scoped_call_expression
    scope: (name) @class_name
    name: (name) @for_method
    (#eq? @class_name "Feature")
    (#eq? @for_method "for"))
  name: (name) @feature_method_name
  arguments: (arguments
    .
    (argument
      (class_constant_access_expression
        (qualified_name) @feature_class_name
        (name) @constant_name)))
  (#match? @feature_method_name "^(active|inactive|value|when)$")
  (#eq? @constant_name "class"))

; ============================================================================
; Pattern 31: Middleware alias definitions in $middlewareAliases property
; ============================================================================
; Matches: protected $middlewareAliases = ['auth' => Authenticate::class];
;          protected $routeMiddleware = ['guest' => RedirectIfAuthenticated::class];
;
; These are defined in app/Http/Kernel.php and map string aliases to classes
;
; AST structure:
;   property_declaration
;     property_element
;       name: variable_name
;       default_value: array_creation_expression
;         array_element_initializer
;           string > string_content (the alias key)
;           class_constant_access_expression
;             name (class name)
;             name ("class")

; Single-quoted alias key
(property_declaration
  (property_element
    name: (variable_name) @mw_def_property
    default_value: (array_creation_expression
      (array_element_initializer
        (string
          (string_content) @middleware_alias_key)
        (class_constant_access_expression
          .
          (name) @middleware_alias_class))))
  (#match? @mw_def_property "\\$(middlewareAliases|routeMiddleware)"))

; Double-quoted alias key
(property_declaration
  (property_element
    name: (variable_name) @mw_def_property
    default_value: (array_creation_expression
      (array_element_initializer
        (encapsed_string
          (string_content) @middleware_alias_key)
        (class_constant_access_expression
          .
          (name) @middleware_alias_class))))
  (#match? @mw_def_property "\\$(middlewareAliases|routeMiddleware)"))

; Qualified (namespaced) class name with single-quoted key
(property_declaration
  (property_element
    name: (variable_name) @mw_def_property
    default_value: (array_creation_expression
      (array_element_initializer
        (string
          (string_content) @middleware_alias_key)
        (class_constant_access_expression
          .
          (qualified_name) @middleware_alias_class))))
  (#match? @mw_def_property "\\$(middlewareAliases|routeMiddleware)"))

; Qualified (namespaced) class name with double-quoted key
(property_declaration
  (property_element
    name: (variable_name) @mw_def_property
    default_value: (array_creation_expression
      (array_element_initializer
        (encapsed_string
          (string_content) @middleware_alias_key)
        (class_constant_access_expression
          .
          (qualified_name) @middleware_alias_class))))
  (#match? @mw_def_property "\\$(middlewareAliases|routeMiddleware)"))

; ============================================================================
; Pattern 32: Middleware group definitions in $middlewareGroups property
; ============================================================================
; Matches: protected $middlewareGroups = ['web' => [...]];
;          protected $middlewareGroups = ['api' => [...]];
;
; Groups contain arrays of middleware classes, not single class references

; Single-quoted group key with array value
(property_declaration
  (property_element
    name: (variable_name) @mw_group_property
    default_value: (array_creation_expression
      (array_element_initializer
        (string
          (string_content) @middleware_group_key)
        (array_creation_expression))))
  (#eq? @mw_group_property "$middlewareGroups"))

; Double-quoted group key with array value
(property_declaration
  (property_element
    name: (variable_name) @mw_group_property
    default_value: (array_creation_expression
      (array_element_initializer
        (encapsed_string
          (string_content) @middleware_group_key)
        (array_creation_expression))))
  (#eq? @mw_group_property "$middlewareGroups"))

; ============================================================================
; Pattern 33: Method body array assignment for Laravel 11+ Middleware.php
; ============================================================================
; Matches: $aliases = ['auth' => Authenticate::class, ...]
; Matches: $middleware = ['web' => [...], 'api' => [...]]
;
; This captures middleware definitions in defaultAliases() and getMiddlewareGroups()
; methods where the array is assigned to a local variable

; Single-quoted key with class constant value (for $aliases)
(assignment_expression
  left: (variable_name) @_mw_assign_var
  right: (array_creation_expression
    (array_element_initializer
      (string
        (string_content) @middleware_alias_key)
      (class_constant_access_expression
        .
        (name) @middleware_alias_class)))
  (#match? @_mw_assign_var "\\$aliases"))

; Double-quoted key with class constant value (for $aliases)
(assignment_expression
  left: (variable_name) @_mw_assign_var
  right: (array_creation_expression
    (array_element_initializer
      (encapsed_string
        (string_content) @middleware_alias_key)
      (class_constant_access_expression
        .
        (name) @middleware_alias_class)))
  (#match? @_mw_assign_var "\\$aliases"))

; Single-quoted key with qualified class constant (for $aliases)
(assignment_expression
  left: (variable_name) @_mw_assign_var
  right: (array_creation_expression
    (array_element_initializer
      (string
        (string_content) @middleware_alias_key)
      (class_constant_access_expression
        .
        (qualified_name) @middleware_alias_class)))
  (#match? @_mw_assign_var "\\$aliases"))

; Double-quoted key with qualified class constant (for $aliases)
(assignment_expression
  left: (variable_name) @_mw_assign_var
  right: (array_creation_expression
    (array_element_initializer
      (encapsed_string
        (string_content) @middleware_alias_key)
      (class_constant_access_expression
        .
        (qualified_name) @middleware_alias_class)))
  (#match? @_mw_assign_var "\\$aliases"))

; Single-quoted key with array value (for $middleware groups like 'web', 'api')
(assignment_expression
  left: (variable_name) @_mw_assign_var
  right: (array_creation_expression
    (array_element_initializer
      (string
        (string_content) @middleware_group_key)
      (array_creation_expression)))
  (#match? @_mw_assign_var "\\$middleware"))

; Double-quoted key with array value (for $middleware groups)
(assignment_expression
  left: (variable_name) @_mw_assign_var
  right: (array_creation_expression
    (array_element_initializer
      (encapsed_string
        (string_content) @middleware_group_key)
      (array_creation_expression)))
  (#match? @_mw_assign_var "\\$middleware"))

; ============================================================================
; Pattern 34: $middleware->alias('name', Class::class) - single alias
; ============================================================================
; Matches: $middleware->alias('auth', Authenticate::class)

; Single-quoted alias name with simple class name
(member_call_expression
  object: (variable_name) @_mw_obj
  name: (name) @_mw_method
  arguments: (arguments
    (argument
      (string
        (string_content) @middleware_alias_key))
    (argument
      (class_constant_access_expression
        .
        (name) @middleware_alias_class)))
  (#eq? @_mw_method "alias")
  (#match? @_mw_obj "\\$middleware"))

; Single-quoted alias name with qualified class name
(member_call_expression
  object: (variable_name) @_mw_obj
  name: (name) @_mw_method
  arguments: (arguments
    (argument
      (string
        (string_content) @middleware_alias_key))
    (argument
      (class_constant_access_expression
        .
        (qualified_name) @middleware_alias_class)))
  (#eq? @_mw_method "alias")
  (#match? @_mw_obj "\\$middleware"))

; Double-quoted alias name with simple class name
(member_call_expression
  object: (variable_name) @_mw_obj
  name: (name) @_mw_method
  arguments: (arguments
    (argument
      (encapsed_string
        (string_content) @middleware_alias_key))
    (argument
      (class_constant_access_expression
        .
        (name) @middleware_alias_class)))
  (#eq? @_mw_method "alias")
  (#match? @_mw_obj "\\$middleware"))

; Double-quoted alias name with qualified class name
(member_call_expression
  object: (variable_name) @_mw_obj
  name: (name) @_mw_method
  arguments: (arguments
    (argument
      (encapsed_string
        (string_content) @middleware_alias_key))
    (argument
      (class_constant_access_expression
        .
        (qualified_name) @middleware_alias_class)))
  (#eq? @_mw_method "alias")
  (#match? @_mw_obj "\\$middleware"))

; ============================================================================
; Pattern 35: $middleware->alias(['key' => Class::class, ...]) - array of aliases
; ============================================================================
; Matches: $middleware->alias(['auth' => Authenticate::class, 'guest' => ...])

; Single-quoted keys in array argument
(member_call_expression
  object: (variable_name) @_mw_obj
  name: (name) @_mw_method
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (string
            (string_content) @middleware_alias_key)
          (class_constant_access_expression
            .
            (name) @middleware_alias_class)))))
  (#eq? @_mw_method "alias")
  (#match? @_mw_obj "\\$middleware"))

; Single-quoted keys with qualified class names in array argument
(member_call_expression
  object: (variable_name) @_mw_obj
  name: (name) @_mw_method
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (string
            (string_content) @middleware_alias_key)
          (class_constant_access_expression
            .
            (qualified_name) @middleware_alias_class)))))
  (#eq? @_mw_method "alias")
  (#match? @_mw_obj "\\$middleware"))

; ============================================================================
; Pattern 36: $middleware->group('name', [...]) - group definition
; ============================================================================
; Matches: $middleware->group('custom', [FirstMiddleware::class, ...])

; Single-quoted group name
(member_call_expression
  object: (variable_name) @_mw_obj
  name: (name) @_mw_method
  arguments: (arguments
    (argument
      (string
        (string_content) @middleware_group_key))
    (argument
      (array_creation_expression)))
  (#eq? @_mw_method "group")
  (#match? @_mw_obj "\\$middleware"))

; Double-quoted group name
(member_call_expression
  object: (variable_name) @_mw_obj
  name: (name) @_mw_method
  arguments: (arguments
    (argument
      (encapsed_string
        (string_content) @middleware_group_key))
    (argument
      (array_creation_expression)))
  (#eq? @_mw_method "group")
  (#match? @_mw_obj "\\$middleware"))

; ============================================================================
; Pattern 37: $middleware->appendToGroup() / prependToGroup() calls
; ============================================================================
; Matches: $middleware->appendToGroup('web', CustomMiddleware::class)
; Matches: $middleware->prependToGroup('api', [First::class, Second::class])
;
; These calls don't define new groups - they just confirm the group exists
; So we capture the group name to validate it's a known group

; appendToGroup with single-quoted group name
(member_call_expression
  object: (variable_name) @_mw_obj
  name: (name) @_mw_method
  arguments: (arguments
    (argument
      (string
        (string_content) @middleware_group_key)))
  (#match? @_mw_method "(appendToGroup|prependToGroup)")
  (#match? @_mw_obj "\\$middleware"))

; appendToGroup with double-quoted group name
(member_call_expression
  object: (variable_name) @_mw_obj
  name: (name) @_mw_method
  arguments: (arguments
    (argument
      (encapsed_string
        (string_content) @middleware_group_key)))
  (#match? @_mw_method "(appendToGroup|prependToGroup)")
  (#match? @_mw_obj "\\$middleware"))

; ============================================================================
; Pattern 38: $router->aliasMiddleware() / middlewareGroup() - service providers
; ============================================================================
; Matches: $router->aliasMiddleware('auth', Authenticate::class)
; Matches: $router->middlewareGroup('custom', [...])

; aliasMiddleware with single-quoted alias and simple class
(member_call_expression
  object: (variable_name) @_router_obj
  name: (name) @_router_method
  arguments: (arguments
    (argument
      (string
        (string_content) @middleware_alias_key))
    (argument
      (class_constant_access_expression
        .
        (name) @middleware_alias_class)))
  (#eq? @_router_method "aliasMiddleware")
  (#match? @_router_obj "\\$router"))

; aliasMiddleware with single-quoted alias and qualified class
(member_call_expression
  object: (variable_name) @_router_obj
  name: (name) @_router_method
  arguments: (arguments
    (argument
      (string
        (string_content) @middleware_alias_key))
    (argument
      (class_constant_access_expression
        .
        (qualified_name) @middleware_alias_class)))
  (#eq? @_router_method "aliasMiddleware")
  (#match? @_router_obj "\\$router"))

; middlewareGroup with single-quoted group name
(member_call_expression
  object: (variable_name) @_router_obj
  name: (name) @_router_method
  arguments: (arguments
    (argument
      (string
        (string_content) @middleware_group_key))
    (argument
      (array_creation_expression)))
  (#eq? @_router_method "middlewareGroup")
  (#match? @_router_obj "\\$router"))

; ============================================================================
; Pattern 38.5: $blade->component() / Blade::component() - service providers
; ============================================================================
; Matches: $blade->component('components.buttons.light-button', 'light-button')
; Matches: Blade::component('components.alert', 'alert')
; Matches: \Illuminate\Support\Facades\Blade::component('a.b', 'c')
;
; First argument is the view path (dot notation) or PHP class name.
; Second argument is the alias used in the <x-{alias}> tag.

; Instance form, single-quoted args: $blade->component('view.path', 'alias')
(member_call_expression
  object: (variable_name) @_blade_obj
  name: (name) @_blade_method
  arguments: (arguments
    (argument
      (string
        (string_content) @blade_alias_view))
    (argument
      (string
        (string_content) @blade_alias_name)))
  (#eq? @_blade_method "component")
  (#match? @_blade_obj "\\$blade"))

; Instance form, double-quoted args
(member_call_expression
  object: (variable_name) @_blade_obj
  name: (name) @_blade_method
  arguments: (arguments
    (argument
      (encapsed_string
        (string_content) @blade_alias_view))
    (argument
      (encapsed_string
        (string_content) @blade_alias_name)))
  (#eq? @_blade_method "component")
  (#match? @_blade_obj "\\$blade"))

; Static form (Blade::component or fully qualified), single-quoted args
(scoped_call_expression
  scope: (name) @_blade_class
  name: (name) @_blade_method
  arguments: (arguments
    (argument
      (string
        (string_content) @blade_alias_view))
    (argument
      (string
        (string_content) @blade_alias_name)))
  (#eq? @_blade_method "component")
  (#match? @_blade_class "(^|\\\\)Blade$"))

; Static form, qualified class scope, single-quoted args
(scoped_call_expression
  scope: (qualified_name) @_blade_class
  name: (name) @_blade_method
  arguments: (arguments
    (argument
      (string
        (string_content) @blade_alias_view))
    (argument
      (string
        (string_content) @blade_alias_name)))
  (#eq? @_blade_method "component")
  (#match? @_blade_class ".*Blade$"))

; ============================================================================
; Pattern 39: Feature class $name property for custom aliases
; ============================================================================
; Matches: public string $name = 'custom-alias';
;          public $name = 'custom-alias';
;          protected string $name = "custom-alias";
;
; Used to detect custom feature aliases in Laravel Pennant feature classes.
; When Feature::active('custom-alias') is used, this finds the class that
; defines $name = 'custom-alias' instead of just deriving from class name.

; Typed property with single-quoted string value
(property_declaration
  (property_element
    name: (variable_name) @_feature_name_prop
    default_value: (string
      (string_content) @feature_name_value))
  (#eq? @_feature_name_prop "$name"))

; Typed property with double-quoted string value
(property_declaration
  (property_element
    name: (variable_name) @_feature_name_prop
    default_value: (encapsed_string
      (string_content) @feature_name_value))
  (#eq? @_feature_name_prop "$name"))

; ============================================================================
; Pattern 40: Property-form member access ($user->email, $this->profile)
; ============================================================================
; Matches: $user->email          (potential accessor / column)
;          $user->posts          (potential relationship)
;          $this->profile        (typed-property access)
;          $user?->name          (nullsafe form)
;
; Captures the member NAME node; the receiver expression (object) and the
; nullsafe-ness are read from its parent in `extract_all_php_patterns`. This
; is the property-form half of the magic-member capture (M2 of the semantic-
; index plan). Receiver resolution + classification happen later (M3); this
; query is the raw capture only.
;
; `name: (name)` restricts to static identifiers — dynamic access like
; `$user->$prop` (name is a variable) is intentionally excluded.

(member_access_expression
  name: (name) @member_access_name)

(nullsafe_member_access_expression
  name: (name) @member_access_name)

; ============================================================================
; Pattern 41: Call-form member access ($user->active(), User::whereEmail())
; ============================================================================
; Matches: $user->active()       (potential scope, instance form)
;          $user->posts()        (relationship called as a method)
;          User::active()        (potential scope, static form)
;          User::whereEmail(...) (potential dynamic finder)
;          $user?->posts()       (nullsafe form)
;
; The call-form half of the magic-member capture (#77). Captures the method
; NAME node; the receiver (`object` for instance calls, `scope` for static
; calls) is read from the parent in `extract_all_php_patterns`. Classification
; prunes the firehose: a call only indexes when its receiver resolves to a
; known class AND the member classifies as a scope / dynamic finder /
; relationship — plain method calls are dropped (Intelephense's territory).
; `self::` / `static::` receivers arrive as `relative_scope` nodes and simply
; fail receiver resolution.

(member_call_expression
  name: (name) @member_call_name)

(nullsafe_member_call_expression
  name: (name) @member_call_name)

(scoped_call_expression
  name: (name) @scoped_call_name)
