use crate::LaravelLanguageServer;

/// `<x-test::` must be recognized as a Blade component context so namespaced
/// components get completion — the `:` used to be rejected, killing the menu.
#[test]
fn namespaced_component_is_a_completion_context() {
    let line = "<x-test::";
    let ctx = LaravelLanguageServer::get_blade_component_context(line, line.len() as u32)
        .expect("`<x-test::` should be a component context");
    assert_eq!(ctx.prefix, "test::");
    assert_eq!(ctx.start_col, 3, "replacement starts right after `<x-`");
}

#[test]
fn namespaced_component_with_partial_name() {
    let line = "    <x-test::back";
    let ctx = LaravelLanguageServer::get_blade_component_context(line, line.len() as u32)
        .expect("partial namespaced name should still be a context");
    assert_eq!(ctx.prefix, "test::back");
}

#[test]
fn plain_component_still_detected() {
    // Regression: allowing `:` must not break the non-namespaced case.
    let line = "<x-button";
    let ctx = LaravelLanguageServer::get_blade_component_context(line, line.len() as u32)
        .expect("plain component context");
    assert_eq!(ctx.prefix, "button");
}

#[test]
fn dotted_component_still_detected() {
    let line = "<x-forms.input";
    let ctx = LaravelLanguageServer::get_blade_component_context(line, line.len() as u32)
        .expect("dotted component context");
    assert_eq!(ctx.prefix, "forms.input");
}

#[test]
fn space_after_tag_name_ends_the_context() {
    // Once attributes begin, we're past the component name.
    let line = "<x-test::backstage ";
    assert!(
        LaravelLanguageServer::get_blade_component_context(line, line.len() as u32).is_none(),
        "a space (start of attributes) must end the component-name context",
    );
}
