//! Model Analyzer for Eloquent Model Autocomplete
//!
//! Parses Laravel Eloquent model files to extract:
//! - Table name
//! - Casts (attribute type overrides)
//! - Accessors (computed properties)
//! - Relationships (belongsTo, hasMany, etc.)

use regex::Regex;
use std::collections::HashMap;

/// Source of a model property
#[derive(Debug, Clone, PartialEq)]
pub enum PropertySource {
    /// From database table column
    Database,
    /// From $casts property
    Cast,
    /// From accessor method (get*Attribute or Attribute return)
    Accessor,
    /// From relationship method
    Relationship,
}

/// A model property with type information
#[derive(Debug, Clone)]
pub struct ModelProperty {
    /// Property name (snake_case for database/casts, as-is for accessors)
    pub name: String,
    /// PHP type (int, string, Carbon, array, Model class, Collection, etc.)
    pub php_type: String,
    /// Where this property comes from
    pub source: PropertySource,
    /// Optional documentation/description
    pub documentation: Option<String>,
}

/// Accessor information extracted from model
#[derive(Debug, Clone)]
pub struct AccessorInfo {
    /// Property name (snake_case)
    pub property_name: String,
    /// Return type if specified
    pub return_type: Option<String>,
    /// Whether this is a new-style Attribute accessor
    pub is_attribute_style: bool,
}

/// Relationship information extracted from model
#[derive(Debug, Clone)]
pub struct RelationshipInfo {
    /// Method name (camelCase)
    pub method_name: String,
    /// Relationship type (hasMany, belongsTo, etc.)
    pub relationship_type: String,
    /// Related model class
    pub related_model: Option<String>,
}

/// Extracted metadata from a model file
#[derive(Debug, Clone, Default)]
pub struct ModelMetadata {
    /// Class name of the model
    pub class_name: String,
    /// Table name if specified ($table property)
    pub table_name: Option<String>,
    /// Casts: property_name -> cast_type
    pub casts: HashMap<String, String>,
    /// Accessors found in the model
    pub accessors: Vec<AccessorInfo>,
    /// Relationships found in the model
    pub relationships: Vec<RelationshipInfo>,
}

impl ModelMetadata {
    /// Parse a model file and extract metadata
    pub fn from_content(content: &str) -> Self {
        let class_name = Self::extract_class_name(content).unwrap_or_default();
        let table_name = Self::extract_table_name(content);
        let casts = Self::extract_casts(content);
        let accessors = Self::extract_accessors(content);
        let relationships = Self::extract_relationships(content);

        Self {
            class_name,
            table_name,
            casts,
            accessors,
            relationships,
        }
    }

    /// Extract the class name from the model file
    fn extract_class_name(content: &str) -> Option<String> {
        let re = Regex::new(r"class\s+(\w+)\s+extends").ok()?;
        re.captures(content)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
    }

    /// Extract $table = 'table_name' from the model
    fn extract_table_name(content: &str) -> Option<String> {
        // Match: protected $table = 'table_name';
        let re = Regex::new(r#"(?:protected|public)\s+\$table\s*=\s*['"]([^'"]+)['"]"#).ok()?;
        re.captures(content)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
    }

    /// Extract casts from $casts property or casts() method
    fn extract_casts(content: &str) -> HashMap<String, String> {
        let mut casts = HashMap::new();

        // Match $casts = [...] property
        if let Some(property_casts) = Self::extract_casts_property(content) {
            casts.extend(property_casts);
        }

        // Match casts(): array method (Laravel 11+ style)
        if let Some(method_casts) = Self::extract_casts_method(content) {
            casts.extend(method_casts);
        }

        casts
    }

    /// Extract casts from $casts = [...] property
    fn extract_casts_property(content: &str) -> Option<HashMap<String, String>> {
        // Find the $casts array
        let re = Regex::new(r"(?s)\$casts\s*=\s*\[([^\]]*)\]").ok()?;
        let caps = re.captures(content)?;
        let array_content = caps.get(1)?.as_str();

        Some(Self::parse_cast_array(array_content))
    }

    /// Extract casts from casts(): array method
    fn extract_casts_method(content: &str) -> Option<HashMap<String, String>> {
        // Find the casts() method and its return array
        let re =
            Regex::new(r"(?s)function\s+casts\s*\(\s*\)\s*:\s*array\s*\{\s*return\s*\[([^\]]*)\]")
                .ok()?;
        let caps = re.captures(content)?;
        let array_content = caps.get(1)?.as_str();

        Some(Self::parse_cast_array(array_content))
    }

    /// Parse a PHP array of casts: 'key' => 'value' or 'key' => CastClass::class
    fn parse_cast_array(content: &str) -> HashMap<String, String> {
        let mut casts = HashMap::new();

        // Match 'key' => 'value' or "key" => "value"
        let string_re = Regex::new(r#"['"](\w+)['"]\s*=>\s*['"]([^'"]+)['"]"#).unwrap();
        for caps in string_re.captures_iter(content) {
            if let (Some(key), Some(value)) = (caps.get(1), caps.get(2)) {
                casts.insert(key.as_str().to_string(), value.as_str().to_string());
            }
        }

        // Match 'key' => SomeClass::class
        let class_re = Regex::new(r#"['"](\w+)['"]\s*=>\s*(\w+)::class"#).unwrap();
        for caps in class_re.captures_iter(content) {
            if let (Some(key), Some(class)) = (caps.get(1), caps.get(2)) {
                casts.insert(key.as_str().to_string(), class.as_str().to_string());
            }
        }

        casts
    }

    /// Extract accessor methods from the model
    fn extract_accessors(content: &str) -> Vec<AccessorInfo> {
        let mut accessors = Vec::new();

        // Old-style: getFirstNameAttribute(): string
        let old_style_re =
            Regex::new(r"(?:public\s+)?function\s+get(\w+)Attribute\s*\([^)]*\)\s*(?::\s*(\w+))?")
                .unwrap();

        for caps in old_style_re.captures_iter(content) {
            if let Some(name) = caps.get(1) {
                let property_name = Self::pascal_to_snake(name.as_str());
                let return_type = caps.get(2).map(|m| m.as_str().to_string());
                accessors.push(AccessorInfo {
                    property_name,
                    return_type,
                    is_attribute_style: false,
                });
            }
        }

        // New-style: firstName(): Attribute
        let new_style_re =
            Regex::new(r"(?:public\s+)?function\s+(\w+)\s*\([^)]*\)\s*:\s*Attribute").unwrap();

        for caps in new_style_re.captures_iter(content) {
            if let Some(name) = caps.get(1) {
                let method_name = name.as_str();
                // New-style accessors use camelCase method names
                let property_name = Self::camel_to_snake(method_name);
                accessors.push(AccessorInfo {
                    property_name,
                    return_type: None, // Type is defined in the Attribute::make() call
                    is_attribute_style: true,
                });
            }
        }

        accessors
    }

    /// Extract relationship methods from the model
    fn extract_relationships(content: &str) -> Vec<RelationshipInfo> {
        let mut relationships = Vec::new();

        // Common relationship types - ordered longest first to avoid partial matches
        let relationship_types = [
            "belongsToMany",
            "belongsTo", // belongsToMany before belongsTo
            "hasManyThrough",
            "hasOneThrough",
            "hasMany",
            "hasOne", // through variants first
            "morphToMany",
            "morphedByMany",
            "morphMany",
            "morphOne",
            "morphTo", // morph variants
        ];

        for rel_type in relationship_types {
            // Match: function methodName(): RelationType (return type style)
            let return_type_pattern = format!(
                r"function\s+(\w+)\s*\([^)]*\)\s*:\s*(?:\w+\\)*{}\b",
                regex::escape(rel_type)
            );

            if let Ok(return_type_re) = Regex::new(&return_type_pattern) {
                for caps in return_type_re.captures_iter(content) {
                    if let Some(method) = caps.get(1) {
                        let method_name = method.as_str().to_string();
                        // Don't add duplicates
                        if !relationships
                            .iter()
                            .any(|r: &RelationshipInfo| r.method_name == method_name)
                        {
                            let related_model = Self::extract_related_model_from_relationship(
                                content,
                                &method_name,
                            );
                            relationships.push(RelationshipInfo {
                                method_name,
                                relationship_type: rel_type.to_string(),
                                related_model,
                            });
                        }
                    }
                }
            }

            // Also match by method body: $this->hasMany(...) etc.
            let body_pattern = format!(
                r"function\s+(\w+)\s*\([^)]*\)[^\{{]*\{{\s*return\s+\$this->{}",
                regex::escape(rel_type)
            );

            if let Ok(body_re) = Regex::new(&body_pattern) {
                for caps in body_re.captures_iter(content) {
                    if let Some(method) = caps.get(1) {
                        let method_name = method.as_str().to_string();
                        // Don't add duplicates
                        if !relationships
                            .iter()
                            .any(|r: &RelationshipInfo| r.method_name == method_name)
                        {
                            let related_model = Self::extract_related_model_from_relationship(
                                content,
                                &method_name,
                            );
                            relationships.push(RelationshipInfo {
                                method_name,
                                relationship_type: rel_type.to_string(),
                                related_model,
                            });
                        }
                    }
                }
            }
        }

        relationships
    }

    /// Extract the related model class from a relationship method
    fn extract_related_model_from_relationship(content: &str, method_name: &str) -> Option<String> {
        // Find the method body and extract the first argument to the relationship call
        // e.g., $this->hasMany(Post::class) -> Post
        let method_re = Regex::new(&format!(
            r"function\s+{}\s*\([^)]*\)[^{{]*\{{\s*return\s+\$this->\w+\(\s*(\w+)::class",
            regex::escape(method_name)
        ))
        .ok()?;

        method_re
            .captures(content)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
    }

    /// Convert PascalCase to snake_case
    /// e.g., FirstName -> first_name
    pub fn pascal_to_snake(s: &str) -> String {
        let mut result = String::new();
        for (i, c) in s.chars().enumerate() {
            if c.is_uppercase() {
                if i > 0 {
                    result.push('_');
                }
                result.push(c.to_lowercase().next().unwrap_or(c));
            } else {
                result.push(c);
            }
        }
        result
    }

    /// Convert camelCase to snake_case
    /// e.g., firstName -> first_name
    fn camel_to_snake(s: &str) -> String {
        Self::pascal_to_snake(s)
    }
}

/// Map cast type to PHP type
pub fn map_cast_to_php_type(cast: &str) -> String {
    let cast_lower = cast.to_lowercase();

    match cast_lower.as_str() {
        // Date/time casts -> Carbon
        "datetime" | "date" | "timestamp" | "immutable_datetime" | "immutable_date" => {
            "Carbon".to_string()
        }
        // Array/collection casts
        "array" | "json" | "collection" | "object" => "array".to_string(),
        // Boolean casts
        "boolean" | "bool" => "bool".to_string(),
        // Integer casts
        "integer" | "int" => "int".to_string(),
        // Float casts
        "float" | "double" | "decimal" | "real" => "float".to_string(),
        // String cast
        "string" => "string".to_string(),
        // Encrypted casts (return the base type)
        "encrypted" => "string".to_string(),
        "encrypted:array" | "encrypted:collection" | "encrypted:object" => "array".to_string(),
        // AsStringable
        "asstringable" => "Stringable".to_string(),
        // AsArrayObject / AsCollection
        "asarrayobject" => "ArrayObject".to_string(),
        "ascollection" => "Collection".to_string(),
        // Custom cast class - return the class name
        _ => cast.to_string(),
    }
}

/// Get the PHP type for a relationship
pub fn relationship_to_php_type(rel_type: &str, related_model: Option<&str>) -> String {
    let model = related_model.unwrap_or("Model");

    match rel_type {
        // Single model relationships
        "hasOne" | "belongsTo" | "morphOne" | "morphTo" | "hasOneThrough" => {
            format!("?{}", model)
        }
        // Collection relationships
        "hasMany" | "belongsToMany" | "morphMany" | "morphToMany" | "morphedByMany"
        | "hasManyThrough" => {
            format!("Collection<{}>", model)
        }
        _ => "mixed".to_string(),
    }
}

#[cfg(test)]
mod tests;
