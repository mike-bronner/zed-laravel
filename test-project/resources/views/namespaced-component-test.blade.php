{{-- Namespaced Blade component resolution fixtures (issue #79). --}}
{{-- Each tag below resolves at runtime in Laravel; none should report --}}
{{-- "Blade component not found". --}}

{{-- Package view namespace + directory-index convention: --}}
{{-- vendor/filament/support/resources/views/components/button/index.blade.php --}}
<x-filament::button>Save</x-filament::button>

{{-- Package view namespace, flat file: --}}
{{-- vendor/filament/support/resources/views/components/badge.blade.php --}}
<x-filament::badge>New</x-filament::badge>

{{-- Markdown mail components live under html/, not components/: --}}
{{-- vendor/laravel/framework/src/Illuminate/Mail/resources/views/html/message.blade.php --}}
<x-mail::message>
    Hello from the mail namespace.
</x-mail::message>

{{-- Livewire v4 default component namespace (config/livewire.php): --}}
{{-- 'layouts' => resource_path('views/layouts') --}}
<x-layouts::app>
    {{-- MaryUI registers class components dynamically with a config prefix: --}}
    {{-- vendor/robsontenorio/mary/src/View/Components/Card.php --}}
    <x-mary-card title="A card" />

    {{-- Negative fixture: must STILL report "component not found". --}}
    <x-filament::does-not-exist />
</x-layouts::app>
