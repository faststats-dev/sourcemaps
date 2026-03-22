use std::collections::HashMap;

use uuid::Uuid;

use crate::error::AppError;
use crate::storage::Storage;

use super::{OriginalPosition, s3_key};

const PROGUARD_FILE_NAME: &str = "proguard/mapping.txt";

/// A parsed proguard mapping file.
struct ProguardMapping {
    /// Maps obfuscated class name -> ClassMapping
    classes: HashMap<String, ClassMapping>,
}

struct ClassMapping {
    original_name: String,
    /// Source file name from comment metadata
    file_name: Option<String>,
    /// Maps obfuscated method name -> Vec of method entries (overloaded methods can have multiple)
    methods: Vec<MethodMapping>,
    /// Maps obfuscated field name -> original field name
    fields: HashMap<String, String>,
}

struct MethodMapping {
    original_name: String,
    obfuscated_name: String,
    start_line: u32,
    end_line: u32,
}

impl ProguardMapping {
    fn parse(input: &str) -> Result<Self, AppError> {
        let mut classes = HashMap::new();
        let mut current_class: Option<(String, ClassMapping)> = None;

        for line in input.lines() {
            let line = line.trim_end();

            // Skip empty lines
            if line.is_empty() {
                continue;
            }

            // Handle comments - check for source file metadata
            if line.starts_with('#') {
                if let Some((_, ref mut class)) = current_class {
                    // Try to extract fileName from JSON comment
                    if let Some(json_start) = line.find('{')
                        && let Ok(meta) =
                            serde_json::from_str::<serde_json::Value>(&line[json_start..])
                        && let Some(file_name) = meta.get("fileName").and_then(|v| v.as_str())
                    {
                        class.file_name = Some(file_name.to_string());
                    }
                }
                continue;
            }

            // Class mapping: no leading whitespace, ends with ':'
            if !line.starts_with(' ') && !line.starts_with('\t') {
                // Save previous class
                if let Some((obfuscated, class)) = current_class.take() {
                    classes.insert(obfuscated, class);
                }

                // Parse: "original.Class -> obfuscated.Class:"
                if let Some((original, obfuscated)) = parse_class_line(line) {
                    current_class = Some((
                        obfuscated,
                        ClassMapping {
                            original_name: original,
                            file_name: None,
                            methods: Vec::new(),
                            fields: HashMap::new(),
                        },
                    ));
                }
                continue;
            }

            // Member mapping (indented)
            if let Some((_, ref mut class)) = current_class {
                let trimmed = line.trim();
                parse_member_line(trimmed, class);
            }
        }

        // Save last class
        if let Some((obfuscated, class)) = current_class {
            classes.insert(obfuscated, class);
        }

        Ok(ProguardMapping { classes })
    }

    fn resolve(
        &self,
        class_name: &str,
        method_name: Option<&str>,
        line: Option<u32>,
    ) -> Result<OriginalPosition, AppError> {
        let class = self.classes.get(class_name).ok_or(AppError::NotFound)?;

        let resolved_method = method_name.and_then(|method| {
            // Find best matching method by line number
            if let Some(line_num) = line {
                class
                    .methods
                    .iter()
                    .find(|m| {
                        m.obfuscated_name == method
                            && line_num >= m.start_line
                            && line_num <= m.end_line
                    })
                    .or_else(|| class.methods.iter().find(|m| m.obfuscated_name == method))
            } else {
                class.methods.iter().find(|m| m.obfuscated_name == method)
            }
        });

        let _source = class.file_name.as_deref().unwrap_or(&class.original_name);

        Ok(OriginalPosition {
            source: class.original_name.clone(),
            line: line.unwrap_or(0),
            column: 0,
            name: resolved_method.map(|m| m.original_name.clone()),
        })
    }
}

/// Parse "original.Class -> obfuscated.Class:" into (original, obfuscated)
fn parse_class_line(line: &str) -> Option<(String, String)> {
    let line = line.strip_suffix(':')?;
    let (original, obfuscated) = line.split_once(" -> ")?;
    Some((original.trim().to_string(), obfuscated.trim().to_string()))
}

/// Parse member lines (methods and fields) and add to class mapping
fn parse_member_line(line: &str, class: &mut ClassMapping) {
    // Try to parse as method with line numbers: "startLine:endLine:returnType method(params) -> obfuscated"
    // Or field: "type fieldName -> obfuscated"
    let Some((original_part, obfuscated)) = line.rsplit_once(" -> ") else {
        return;
    };
    let obfuscated = obfuscated.trim().to_string();

    // Check if it has line number prefixes (method mapping)
    // Format: "29:33:void <init>(java.nio.file.Path,java.nio.charset.Charset,java.lang.Object)"
    // or: "java.nio.file.Path file" (field)
    if let Some(method) = parse_method_with_lines(original_part) {
        class.methods.push(MethodMapping {
            original_name: method.name,
            obfuscated_name: obfuscated,
            start_line: method.start_line,
            end_line: method.end_line,
        });
    } else if original_part.contains('(') {
        // Method without line numbers — no range info to store
    } else {
        // Field mapping: "type fieldName -> obfuscated"
        // We just need the field name (last token before ->)
        let parts: Vec<&str> = original_part.trim().rsplitn(2, ' ').collect();
        if let Some(field_name) = parts.first() {
            class.fields.insert(obfuscated, field_name.to_string());
        }
    }
}

struct ParsedMethod {
    name: String,
    start_line: u32,
    end_line: u32,
}

fn parse_method_with_lines(s: &str) -> Option<ParsedMethod> {
    let s = s.trim();
    // Format: "startLine:endLine:returnType methodName(params)"
    let (start_str, rest) = s.split_once(':')?;
    let start_line: u32 = start_str.parse().ok()?;
    let (end_str, rest) = rest.split_once(':')?;
    let end_line: u32 = end_str.parse().ok()?;

    // rest is "returnType methodName(params)" or "returnType methodName(params):startLine2:endLine2"
    // Handle inlined methods with additional line info
    let rest = if let Some(colon_pos) = rest.find(':') {
        // Check if what follows the colon is digits (inline mapping)
        let after = &rest[colon_pos + 1..];
        if after.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            &rest[..colon_pos]
        } else {
            rest
        }
    } else {
        rest
    };

    // Split "returnType methodName(params)" - find the method name
    let paren_pos = rest.find('(')?;
    let before_paren = &rest[..paren_pos];
    // The method name is the last space-separated token
    let method_name = before_paren
        .rsplit_once(' ')
        .map(|(_, name)| name)
        .unwrap_or(before_paren);

    Some(ParsedMethod {
        name: method_name.to_string(),
        start_line,
        end_line,
    })
}

pub async fn ingest(
    storage: &Storage,
    project_id: Uuid,
    build_id: &str,
    mapping: &str,
) -> Result<(), AppError> {
    let key = s3_key(project_id, build_id, PROGUARD_FILE_NAME);
    storage.put(&key, mapping.as_bytes()).await
}

pub fn apply(
    data: &[u8],
    class_name: &str,
    method_name: Option<&str>,
    line: Option<u32>,
) -> Result<OriginalPosition, AppError> {
    let content = std::str::from_utf8(data)
        .map_err(|e| AppError::BadRequest(format!("invalid proguard mapping: {e}")))?;
    let mapping = ProguardMapping::parse(content)?;
    mapping.resolve(class_name, method_name, line)
}

pub fn proguard_s3_key(project_id: Uuid, build_id: &str) -> String {
    s3_key(project_id, build_id, PROGUARD_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_MAPPING: &str = r#"core.file.FileIO -> a.a.a:
# {"fileName":"FileIO.java","id":"sourceFile"}
    java.nio.file.Path file -> a
    java.nio.charset.Charset charset -> b
    java.lang.Object root -> c
    boolean loaded -> d
    29:33:void <init>(java.nio.file.Path,java.nio.charset.Charset,java.lang.Object) -> <init>
    42:43:void <init>(java.nio.file.Path,java.lang.Object) -> <init>
    52:54:core.file.FileIO setRoot(java.lang.Object) -> a
    65:67:java.lang.Object getRoot() -> a
    java.lang.Object load() -> b
    core.file.FileIO save(java.nio.file.attribute.FileAttribute[]) -> a
    92:92:core.file.FileIO reload() -> c
core.file.Validatable -> a.a.b:
# {"fileName":"Validatable.java","id":"sourceFile"}
    core.file.FileIO validate(core.file.Validatable$Scope) -> a
    26:26:core.file.FileIO validate() -> a_
"#;

    #[test]
    fn parse_class_mappings() {
        let mapping = ProguardMapping::parse(SAMPLE_MAPPING).unwrap();
        assert!(mapping.classes.contains_key("a.a.a"));
        assert!(mapping.classes.contains_key("a.a.b"));

        let file_io = &mapping.classes["a.a.a"];
        assert_eq!(file_io.original_name, "core.file.FileIO");
        assert_eq!(file_io.file_name.as_deref(), Some("FileIO.java"));
    }

    #[test]
    fn parse_field_mappings() {
        let mapping = ProguardMapping::parse(SAMPLE_MAPPING).unwrap();
        let file_io = &mapping.classes["a.a.a"];
        assert_eq!(file_io.fields.get("a"), Some(&"file".to_string()));
        assert_eq!(file_io.fields.get("b"), Some(&"charset".to_string()));
        assert_eq!(file_io.fields.get("d"), Some(&"loaded".to_string()));
    }

    #[test]
    fn parse_method_mappings() {
        let mapping = ProguardMapping::parse(SAMPLE_MAPPING).unwrap();
        let file_io = &mapping.classes["a.a.a"];

        let init_methods: Vec<_> = file_io
            .methods
            .iter()
            .filter(|m| m.obfuscated_name == "<init>")
            .collect();
        assert_eq!(init_methods.len(), 2);
        assert_eq!(init_methods[0].start_line, 29);
        assert_eq!(init_methods[0].end_line, 33);
        assert_eq!(init_methods[1].start_line, 42);
        assert_eq!(init_methods[1].end_line, 43);
    }

    #[test]
    fn resolve_class() {
        let mapping = ProguardMapping::parse(SAMPLE_MAPPING).unwrap();
        let result = mapping.resolve("a.a.a", None, None).unwrap();
        assert_eq!(result.source, "core.file.FileIO");
    }

    #[test]
    fn resolve_method_with_line() {
        let mapping = ProguardMapping::parse(SAMPLE_MAPPING).unwrap();
        let result = mapping.resolve("a.a.a", Some("c"), Some(92)).unwrap();
        assert_eq!(result.source, "core.file.FileIO");
        assert_eq!(result.name.as_deref(), Some("reload"));
    }

    #[test]
    fn resolve_method_without_line() {
        let mapping = ProguardMapping::parse(SAMPLE_MAPPING).unwrap();
        let result = mapping.resolve("a.a.a", Some("a"), None).unwrap();
        assert_eq!(result.source, "core.file.FileIO");
        // Should find one of the methods named "a" (setRoot or getRoot)
        assert!(result.name.is_some());
    }

    #[test]
    fn resolve_unknown_class() {
        let mapping = ProguardMapping::parse(SAMPLE_MAPPING).unwrap();
        let result = mapping.resolve("z.z.z", None, None);
        assert!(result.is_err());
    }
}
