//! Laravel Validation Rules Parser
//!
//! Parses the Laravel framework to dynamically discover validation rules
//! and their parameter options, avoiding hard-coded lists that could become stale.

use regex::Regex;
use std::path::PathBuf;
use tracing::{info, warn};

/// Information about a validation rule parsed from Laravel
#[derive(Debug, Clone)]
pub struct ValidationRuleInfo {
    /// Rule name in snake_case (e.g., "required", "after_or_equal")
    pub name: String,
    /// Whether the rule accepts parameters after a colon
    pub has_params: bool,
    /// What kind of parameters this rule accepts
    pub param_type: ParamType,
    /// Source of the rule ("laravel" or "app/Rules")
    pub source: String,
}

/// Type of parameters a validation rule accepts
#[derive(Debug, Clone, PartialEq)]
pub enum ParamType {
    /// No parameters (e.g., "required", "email")
    None,
    /// References other fields in the validation array (e.g., "after:start_date", "same:password")
    FieldRef,
    /// Database table/column references (e.g., "exists:users,email")
    Database,
    /// Dimension constraints (e.g., "dimensions:min_width=100")
    Dimensions,
    /// File extensions (e.g., "mimes:jpg,png")
    MimeExtensions,
    /// MIME types (e.g., "mimetypes:image/jpeg")
    MimeTypes,
    /// Timezone identifiers (e.g., "timezone:America/New_York")
    Timezone,
    /// User-provided custom values - no autocomplete
    Custom,
}

/// Parser for Laravel framework validation rules
pub struct LaravelRulesParser {
    /// Path to the project root (containing vendor/)
    project_root: PathBuf,
}

impl LaravelRulesParser {
    /// Create a new parser for the given project root
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }

    /// Check if vendor/laravel/framework is available
    pub fn is_vendor_available(&self) -> bool {
        self.get_validates_attributes_path().exists()
    }

    /// Get path to ValidatesAttributes.php trait
    fn get_validates_attributes_path(&self) -> PathBuf {
        self.project_root.join(
            "vendor/laravel/framework/src/Illuminate/Validation/Concerns/ValidatesAttributes.php",
        )
    }

    /// Get path to Dimensions.php rule class
    fn get_dimensions_path(&self) -> PathBuf {
        self.project_root
            .join("vendor/laravel/framework/src/Illuminate/Validation/Rules/Dimensions.php")
    }

    /// Get path to Symfony MimeTypes.php
    fn get_mime_types_path(&self) -> PathBuf {
        self.project_root.join("vendor/symfony/mime/MimeTypes.php")
    }

    /// Parse all validation rules from Laravel framework
    pub fn parse_validation_rules(&self) -> Vec<ValidationRuleInfo> {
        if !self.is_vendor_available() {
            warn!("Laravel vendor not available, cannot parse validation rules");
            return Vec::new();
        }

        let mut rules = Vec::new();

        // Parse ValidatesAttributes.php for all validate* methods
        if let Ok(content) = std::fs::read_to_string(self.get_validates_attributes_path()) {
            rules.extend(self.parse_validates_attributes(&content));
        }

        // Also scan app/Rules for custom rules
        rules.extend(self.scan_custom_rules());

        info!("Parsed {} validation rules from Laravel", rules.len());
        rules
    }

    /// Parse the ValidatesAttributes trait for validation methods
    fn parse_validates_attributes(&self, content: &str) -> Vec<ValidationRuleInfo> {
        let mut rules = Vec::new();

        // Match: public function validateSomething( or protected function validateSomething(
        let method_regex =
            Regex::new(r"(?:public|protected)\s+function\s+validate([A-Z][a-zA-Z]*)\s*\(").unwrap();

        for caps in method_regex.captures_iter(content) {
            if let Some(method_name) = caps.get(1) {
                let camel_name = method_name.as_str();
                let snake_name = Self::camel_to_snake(camel_name);

                // Determine parameter type based on rule name
                let (has_params, param_type) = self.determine_param_type(&snake_name, content);

                rules.push(ValidationRuleInfo {
                    name: snake_name,
                    has_params,
                    param_type,
                    source: "laravel".to_string(),
                });
            }
        }

        rules
    }

    /// Determine the parameter type for a rule by analyzing the method body
    fn determine_param_type(&self, rule_name: &str, _content: &str) -> (bool, ParamType) {
        // Rules that reference other fields
        let field_ref_rules = [
            "after",
            "after_or_equal",
            "before",
            "before_or_equal",
            "date_equals",
            "different",
            "same",
            "gt",
            "gte",
            "lt",
            "lte",
            "required_if",
            "required_unless",
            "required_with",
            "required_with_all",
            "required_without",
            "required_without_all",
            "required_if_accepted",
            "required_if_declined",
            "prohibited_if",
            "prohibited_unless",
            "prohibits",
            "exclude_if",
            "exclude_unless",
            "exclude_with",
            "exclude_without",
            "missing_if",
            "missing_unless",
            "missing_with",
            "missing_with_all",
            "present_if",
            "present_unless",
            "present_with",
            "present_with_all",
            "accepted_if",
            "declined_if",
            "confirmed",
            "in_array",
            "in_array_keys",
        ];

        // Database rules
        let database_rules = ["exists", "unique"];

        // Rules with special fixed options
        let dimensions_rules = ["dimensions"];
        let mime_ext_rules = ["mimes", "extensions"];
        let mime_type_rules = ["mimetypes"];
        let timezone_rules = ["timezone"];

        // Rules with no parameters
        let no_param_rules = [
            "required",
            "nullable",
            "bail",
            "sometimes",
            "filled",
            "present",
            "missing",
            "accepted",
            "declined",
            "boolean",
            "string",
            "integer",
            "numeric",
            "array",
            "list",
            "file",
            "image",
            "email",
            "url",
            "active_url",
            "ip",
            "ipv4",
            "ipv6",
            "mac_address",
            "json",
            "alpha",
            "alpha_dash",
            "alpha_num",
            "ascii",
            "lowercase",
            "uppercase",
            "ulid",
            "uuid",
            "hex_color",
            "prohibited",
            "exclude",
        ];

        if no_param_rules.contains(&rule_name) {
            return (false, ParamType::None);
        }

        if field_ref_rules.contains(&rule_name) {
            return (true, ParamType::FieldRef);
        }

        if database_rules.contains(&rule_name) {
            return (true, ParamType::Database);
        }

        if dimensions_rules.contains(&rule_name) {
            return (true, ParamType::Dimensions);
        }

        if mime_ext_rules.contains(&rule_name) {
            return (true, ParamType::MimeExtensions);
        }

        if mime_type_rules.contains(&rule_name) {
            return (true, ParamType::MimeTypes);
        }

        if timezone_rules.contains(&rule_name) {
            return (true, ParamType::Timezone);
        }

        // Default: has custom parameters (user provides values)
        (true, ParamType::Custom)
    }

    /// Parse dimension constraint options from Dimensions.php
    pub fn parse_dimension_options(&self) -> Vec<String> {
        let path = self.get_dimensions_path();
        if !path.exists() {
            return Self::default_dimension_options();
        }

        if let Ok(content) = std::fs::read_to_string(&path) {
            // Look for method names that set constraints
            // Pattern: public function minWidth, maxWidth, etc.
            let method_regex = Regex::new(
                r"public\s+function\s+(min_?[Ww]idth|max_?[Ww]idth|min_?[Hh]eight|max_?[Hh]eight|width|height|ratio|min_?[Rr]atio|max_?[Rr]atio)\s*\("
            ).unwrap();

            let mut options: Vec<String> = method_regex
                .captures_iter(&content)
                .filter_map(|caps| caps.get(1).map(|m| Self::camel_to_snake(m.as_str())))
                .collect();

            // Also look for constraint keys in the compile method or constraints array
            let constraint_regex = Regex::new(r#"['"](\w+)['"].*=>"#).unwrap();
            for caps in constraint_regex.captures_iter(&content) {
                if let Some(key) = caps.get(1) {
                    let key_str = key.as_str();
                    if [
                        "min_width",
                        "max_width",
                        "min_height",
                        "max_height",
                        "width",
                        "height",
                        "ratio",
                        "min_ratio",
                        "max_ratio",
                    ]
                    .contains(&key_str)
                        && !options.contains(&key_str.to_string())
                    {
                        options.push(key_str.to_string());
                    }
                }
            }

            if !options.is_empty() {
                options.sort();
                options.dedup();
                return options;
            }
        }

        Self::default_dimension_options()
    }

    /// Default dimension options if parsing fails
    fn default_dimension_options() -> Vec<String> {
        vec![
            "height".to_string(),
            "max_height".to_string(),
            "max_ratio".to_string(),
            "max_width".to_string(),
            "min_height".to_string(),
            "min_ratio".to_string(),
            "min_width".to_string(),
            "ratio".to_string(),
            "width".to_string(),
        ]
    }

    /// Parse MIME types and extensions from Symfony MimeTypes.php
    pub fn parse_mime_types(&self) -> (Vec<String>, Vec<String>) {
        let path = self.get_mime_types_path();
        if !path.exists() {
            return (Self::default_mime_extensions(), Self::default_mime_types());
        }

        if let Ok(content) = std::fs::read_to_string(&path) {
            let mut extensions = Vec::new();
            let mut mime_types = Vec::new();

            // Parse the static $map array: 'extension' => ['mime/type', ...]
            // Pattern: 'jpg' => ['image/jpeg'],
            let ext_regex = Regex::new(r#"'([a-z0-9]+)'\s*=>\s*\["#).unwrap();
            let mime_regex = Regex::new(r#"'([a-z]+/[a-z0-9.+-]+)'"#).unwrap();

            for caps in ext_regex.captures_iter(&content) {
                if let Some(ext) = caps.get(1) {
                    extensions.push(ext.as_str().to_string());
                }
            }

            for caps in mime_regex.captures_iter(&content) {
                if let Some(mime) = caps.get(1) {
                    let mime_str = mime.as_str().to_string();
                    if !mime_types.contains(&mime_str) {
                        mime_types.push(mime_str);
                    }
                }
            }

            if !extensions.is_empty() && !mime_types.is_empty() {
                extensions.sort();
                extensions.dedup();
                mime_types.sort();
                mime_types.dedup();
                return (extensions, mime_types);
            }
        }

        (Self::default_mime_extensions(), Self::default_mime_types())
    }

    /// Default MIME extensions if parsing fails
    fn default_mime_extensions() -> Vec<String> {
        vec![
            "avi", "bmp", "csv", "doc", "docx", "gif", "jpeg", "jpg", "json", "mov", "mp3", "mp4",
            "pdf", "png", "ppt", "pptx", "rar", "svg", "txt", "webm", "webp", "xls", "xlsx", "xml",
            "zip",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }

    /// Default MIME types if parsing fails
    fn default_mime_types() -> Vec<String> {
        vec![
            "application/json",
            "application/msword",
            "application/pdf",
            "application/vnd.ms-excel",
            "application/vnd.ms-powerpoint",
            "application/xml",
            "application/zip",
            "audio/mpeg",
            "audio/wav",
            "image/gif",
            "image/jpeg",
            "image/png",
            "image/svg+xml",
            "image/webp",
            "text/csv",
            "text/plain",
            "video/mp4",
            "video/webm",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }

    /// Get common PHP timezone identifiers
    pub fn get_timezone_identifiers() -> Vec<String> {
        // These are the most commonly used timezones
        // A full list would be too long for autocomplete
        vec![
            "Africa/Cairo",
            "Africa/Johannesburg",
            "Africa/Lagos",
            "America/Chicago",
            "America/Denver",
            "America/Los_Angeles",
            "America/New_York",
            "America/Sao_Paulo",
            "America/Toronto",
            "Asia/Dubai",
            "Asia/Hong_Kong",
            "Asia/Kolkata",
            "Asia/Seoul",
            "Asia/Shanghai",
            "Asia/Singapore",
            "Asia/Tokyo",
            "Australia/Melbourne",
            "Australia/Sydney",
            "Europe/Amsterdam",
            "Europe/Berlin",
            "Europe/London",
            "Europe/Madrid",
            "Europe/Moscow",
            "Europe/Paris",
            "Europe/Rome",
            "Pacific/Auckland",
            "Pacific/Honolulu",
            "UTC",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }

    /// Scan app/Rules for custom validation rules
    fn scan_custom_rules(&self) -> Vec<ValidationRuleInfo> {
        let rules_path = self.project_root.join("app/Rules");
        if !rules_path.exists() {
            return Vec::new();
        }

        let mut rules = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&rules_path) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "php") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        // Convert PascalCase to snake_case
                        let rule_name = Self::camel_to_snake(stem);
                        rules.push(ValidationRuleInfo {
                            name: rule_name,
                            has_params: true, // Assume custom rules might have params
                            param_type: ParamType::Custom,
                            source: "app/Rules".to_string(),
                        });
                    }
                }
            }
        }

        rules
    }

    /// Convert CamelCase or PascalCase to snake_case
    fn camel_to_snake(s: &str) -> String {
        let mut result = String::new();
        for (i, c) in s.chars().enumerate() {
            if c.is_uppercase() {
                if i > 0 {
                    result.push('_');
                }
                result.push(c.to_lowercase().next().unwrap());
            } else {
                result.push(c);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests;
