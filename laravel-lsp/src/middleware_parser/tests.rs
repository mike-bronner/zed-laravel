use super::*;

#[test]
fn test_resolve_class_to_file() {
    let root = PathBuf::from("/project");

    // Test App namespace
    let result = resolve_class_to_file("App\\Http\\Middleware\\Authenticate", &root);
    assert!(result.is_some());
    let path = result.unwrap();
    assert!(path.ends_with("Authenticate.php"));
    assert!(path.to_string_lossy().contains("app/Http/Middleware"));
}

#[test]
fn test_middleware_base_alias_strips_parameters() {
    // auth:sanctum is the auth alias with sanctum as a guard parameter
    assert_eq!(middleware_base_alias("auth:sanctum"), "auth");
    // throttle takes rate-limit parameters
    assert_eq!(middleware_base_alias("throttle:60,1"), "throttle");
    // can: takes a permission name
    assert_eq!(middleware_base_alias("can:edit,post"), "can");
}

#[test]
fn test_middleware_base_alias_passthrough_when_no_parameters() {
    assert_eq!(middleware_base_alias("auth"), "auth");
    assert_eq!(middleware_base_alias("web"), "web");
    assert_eq!(middleware_base_alias(""), "");
}
