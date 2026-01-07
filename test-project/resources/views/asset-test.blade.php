<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Asset Navigation Test - Laravel LSP</title>

    {{--
    ============================================================================
    @vite Directive Tests - Navigate to resources/ directory
    ============================================================================

    The @vite directive is used to include Vite assets.
    You should be able to Cmd+Click on each path to navigate to the source file.
    --}}

    {{-- Single asset --}}
    @vite('resources/css/app.css')

    {{-- Multiple assets in array --}}
    @vite(['resources/css/app.css', 'resources/js/app.js'])

    {{-- With different quote styles --}}
    @vite(["resources/css/bootstrap.css", "resources/js/bootstrap.js"])

    {{-- Mixed quotes --}}
    @vite(['resources/css/app.css', "resources/js/app.js"])

    {{-- Additional Vite assets --}}
    @vite(['resources/sass/app.scss', 'resources/ts/app.ts'])

    {{-- Images and other assets --}}
    @vite('resources/images/logo.svg')
    @vite(['resources/images/hero.jpg', 'resources/fonts/inter.woff2'])
</head>
<body>
    <div class="container">
        <h1>Laravel LSP Asset Navigation Test</h1>

        {{--
        ============================================================================
        asset() Helper Tests in Blade
        ============================================================================
        --}}

        <img src="{{ asset('images/logo.png') }}" alt="Logo">
        <img src="{{ asset('images/favicon.ico') }}" alt="Icon">

        <link rel="stylesheet" href="{{ asset('css/app.css') }}">
        <script src="{{ asset('js/app.js') }}"></script>

        {{--
        ============================================================================
        Vite::asset() Helper Tests
        ============================================================================
        --}}

        <img src="{{ Vite::asset('resources/images/logo.svg') }}" alt="Vite Logo">
        <img src="{{ Vite::asset('resources/images/banner.png') }}" alt="Banner">

        {{--
        ============================================================================
        Path Helpers in Blade
        ============================================================================
        --}}

        @php
            // These should all support goto navigation
            $configPath = config_path('app.php');
            $viewPath = resource_path('views/welcome.blade.php');
            $publicFile = public_path('index.php');
            $storagePath = storage_path('logs/laravel.log');
            $appPath = app_path('Models/User.php');
            $basePath = base_path('composer.json');
        @endphp

        <div class="paths">
            <p>Config: {{ $configPath }}</p>
            <p>View: {{ $viewPath }}</p>
            <p>Public: {{ $publicFile }}</p>
            <p>Storage: {{ $storagePath }}</p>
            <p>App: {{ $appPath }}</p>
            <p>Base: {{ $basePath }}</p>
        </div>

        {{--
        ============================================================================
        Complex @vite with spacing and formatting
        ============================================================================
        --}}

        @vite([
            'resources/css/app.css',
            'resources/js/app.js',
            'resources/css/admin.css',
            'resources/js/admin.js'
        ])

        {{-- With extra whitespace --}}
        @vite(  [  'resources/css/custom.css'  ,  'resources/js/custom.js'  ]  )

        {{-- Nested in conditionals --}}
        @if (app()->environment('production'))
            @vite(['resources/css/production.css', 'resources/js/production.js'])
        @else
            @vite(['resources/css/development.css', 'resources/js/development.js'])
        @endif

        {{-- In loops --}}
        @foreach(['resources/css/theme1.css', 'resources/css/theme2.css'] as $theme)
            {{-- Navigation should work in array literals too --}}
            @vite($theme)
        @endforeach

    </div>

    {{-- Final multi-asset @vite --}}
    @vite([
        'resources/css/app.css',
        'resources/js/app.js',
        'resources/images/sprite.svg',
        'resources/fonts/montserrat.woff2'
    ])
</body>
</html>
