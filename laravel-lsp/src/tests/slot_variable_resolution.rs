use crate::LaravelLanguageServer;

#[test]
fn test_extract_slot_variable_usages_basic() {
    let content = r#"
<div>
    <h1>{{ $title }}</h1>
    {{ $slot }}
    <footer>{{ $footer }}</footer>
</div>
"#;
    let vars = LaravelLanguageServer::extract_slot_variable_usages(content);

    // $slot is excluded (framework var), $title and $footer should be found
    assert!(vars.iter().any(|(n, _)| n == "title"));
    assert!(vars.iter().any(|(n, _)| n == "footer"));
    assert!(!vars.iter().any(|(n, _)| n == "slot")); // Excluded
}

#[test]
fn test_extract_slot_variable_usages_method_calls() {
    let content = r#"
@if($header->isNotEmpty())
    <header>{{ $header }}</header>
@endif
"#;
    let vars = LaravelLanguageServer::extract_slot_variable_usages(content);
    assert!(vars.iter().any(|(n, _)| n == "header"));
}

#[test]
fn test_extract_slot_variable_usages_excludes_framework() {
    let content = r#"
{{ $errors->first('email') }}
{{ $slot }}
{{ $attributes->merge(['class' => 'btn']) }}
{{ $component->data() }}
"#;
    let vars = LaravelLanguageServer::extract_slot_variable_usages(content);

    // All framework variables should be excluded
    assert!(!vars.iter().any(|(n, _)| n == "errors"));
    assert!(!vars.iter().any(|(n, _)| n == "slot"));
    assert!(!vars.iter().any(|(n, _)| n == "attributes"));
    assert!(!vars.iter().any(|(n, _)| n == "component"));
}

#[test]
fn test_extract_slot_variable_usages_excludes_loop() {
    let content = r#"
@foreach($items as $item)
    {{ $loop->index }}
    {{ $item->name }}
@endforeach
"#;
    let vars = LaravelLanguageServer::extract_slot_variable_usages(content);

    // $loop should be excluded (framework variable)
    // $item is found in echo {{ $item->name }}
    assert!(!vars.iter().any(|(n, _)| n == "loop"));
    assert!(vars.iter().any(|(n, _)| n == "item")); // Found in echo statement
                                                    // Note: $items is not found because it's only in directive, not in echo
}

#[test]
fn test_is_component_file() {
    assert!(LaravelLanguageServer::is_component_file(
        "/app/resources/views/components/button.blade.php"
    ));
    assert!(LaravelLanguageServer::is_component_file(
        "/app/resources/views/components/forms/input.blade.php"
    ));
    assert!(!LaravelLanguageServer::is_component_file(
        "/app/resources/views/welcome.blade.php"
    ));
    assert!(!LaravelLanguageServer::is_component_file(
        "/app/resources/views/layouts/app.blade.php"
    ));
    assert!(!LaravelLanguageServer::is_component_file(
        "/app/app/Http/Controllers/UserController.php"
    ));
}

#[test]
fn test_extract_slot_variable_usages_complex() {
    let content = r#"
@props(['type' => 'info'])

<div class="alert alert-{{ $type }}">
    @if($title ?? false)
        <h4>{{ $title }}</h4>
    @endif

    <div class="alert-body">
        {{ $slot }}
    </div>

    @if($footer->isNotEmpty())
        <div class="alert-footer">
            {{ $footer }}
        </div>
    @endif
</div>
"#;
    let vars = LaravelLanguageServer::extract_slot_variable_usages(content);

    // Should find $type, $title, $footer but not $slot
    assert!(vars.iter().any(|(n, _)| n == "type"));
    assert!(vars.iter().any(|(n, _)| n == "title"));
    assert!(vars.iter().any(|(n, _)| n == "footer"));
    assert!(!vars.iter().any(|(n, _)| n == "slot"));
}
