<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Component Navigation Test - Laravel LSP</title>
</head>
<body>
    <div class="container">
        <h1>Blade Component Navigation Performance Test</h1>

        {{--
        ============================================================================
        Component Navigation Tests
        ============================================================================

        PERFORMANCE TEST:
        Hover over each <x-button> tag below. The highlight should appear INSTANTLY
        without any delay. If you experience a delay, the performance optimization
        failed.

        Cmd+Click should also navigate instantly to the component file.
        --}}

        {{-- Basic component usage --}}
        <x-button>Click Me</x-button>

        {{-- Component with attributes --}}
        <x-button type="submit" class="btn-primary">Submit</x-button>

        {{-- Self-closing component --}}
        <x-button />

        {{-- Multiple components in a row (test rapid hover) --}}
        <div class="btn-group">
            <x-button>First</x-button>
            <x-button>Second</x-button>
            <x-button>Third</x-button>
            <x-button>Fourth</x-button>
            <x-button>Fifth</x-button>
        </div>

        {{-- Components in loops (stress test) --}}
        @for ($i = 0; $i < 10; $i++)
            <x-button>Button {{ $i }}</x-button>
        @endfor
        {{-- Nested components --}}
        <div class="card">
            <div class="card-header">
                <x-button>Header Button</x-button>
            </div>
            <div class="card-body">
                <x-button>Body Button</x-button>
            </div>
            <div class="card-footer">
                <x-button>Footer Button</x-button>
            </div>
        </div>

        {{--
        ============================================================================
        @vite Directive Tests - Individual Asset Navigation
        ============================================================================

        INDIVIDUAL ASSET NAVIGATION TEST:
        Each asset path within the @vite directive should be individually clickable.
        Hover over 'resources/css/app.css' - it should highlight just that path.
        Hover over 'resources/js/app.js' - it should highlight just that path.
        --}}

        {{-- Test individual asset navigation within array --}}
        @vite([
            'resources/css/app.css',
            'resources/js/app.js'
        ])

        {{-- More complex multi-asset directive --}}
        @vite([
            'resources/css/app.css',
            'resources/js/app.js',
            'resources/css/admin.css',
            'resources/js/admin.js',
            'resources/css/custom.css'
        ])

        {{-- Single-line multi-asset --}}
        @vite(['resources/css/app.css', 'resources/js/app.js'])

        {{--
        ============================================================================
        Blade Directives & Brackets Inside HTML Attributes
        ============================================================================

        TEST: Directives and echo statements inside HTML tag attributes should work.
        Cmd+Click on config('app.name') inside the value attribute should navigate.
        @feature directive inside attributes should also be recognized.
        --}}

        {{-- Echo statements in attributes --}}
        <input type="text" value="{{ config('app.name') }}" placeholder="{{ __('messages.placeholder') }}">

        {{-- Directives in attributes --}}
        <div class="container @if($active) bg-blue-500 @endif" data-env="{{ env('APP_ENV') }}">
            Content with conditional classes
        </div>

        {{-- @class directive (common Blade pattern) --}}
        <button @class(['btn', 'btn-primary' => $isPrimary, 'btn-disabled' => $disabled])>
            Styled Button
        </button>

        {{-- Feature flag in attribute values --}}
        <div class="@if (Feature::active('new-design')) new-design @else old-design @endif">
            Feature-flagged styling
        </div>

        {{-- @feature directive in attribute values --}}
        <div class="@feature ('news') beta-badge @endfeature">
            Beta content
        </div>
@fe
        {{--
        ============================================================================
        Expected Behavior:
        ============================================================================

        BEFORE FIX:
        - Hovering over <x-button> had a 500ms+ delay before highlighting
        - Only the @vite directive as a whole was clickable (navigated to first asset)
        - Individual assets in the array were not clickable

        AFTER FIX:
        - Hovering over <x-button> shows instant highlight (< 50ms)
        - Each asset path in @vite is individually clickable
        - Clicking 'resources/css/app.css' navigates to that specific file
        - Clicking 'resources/js/app.js' navigates to that specific file
        --}}

    </div>
</body>
</html>
