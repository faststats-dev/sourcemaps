use std::collections::HashMap;
use std::collections::hash_map::Entry;

use uuid::Uuid;

use crate::error::AppError;
use crate::storage::Storage;

#[cfg(test)]
use super::OriginalPosition;
use super::s3_key;

const PROGUARD_DIR: &str = "proguard";

struct ProguardMapping {
    classes: HashMap<String, ClassMapping>,
}

struct ClassMapping {
    original_name: String,
    file_name: Option<String>,
    methods: Vec<MethodMapping>,
    fields: HashMap<String, String>,
}

struct MethodMapping {
    original_name: String,
    obfuscated_name: String,
    start_line: Option<u32>,
    end_line: Option<u32>,
}

impl ProguardMapping {
    fn parse(input: &str) -> Result<Self, AppError> {
        let mut classes = HashMap::new();
        let mut current_class: Option<(String, ClassMapping)> = None;

        for line in input.lines() {
            let line = line.trim_end();
            if line.is_empty() {
                continue;
            }

            if line.starts_with('#') {
                if let Some((_, class)) = current_class.as_mut()
                    && let Some(file_name) = extract_file_name(line)
                {
                    class.file_name = Some(file_name);
                }
                continue;
            }

            if !line.starts_with(' ') && !line.starts_with('\t') {
                if let Some((obfuscated, class)) = current_class.take() {
                    classes.insert(obfuscated, class);
                }

                if let Some((original, obfuscated)) = parse_class_line(line) {
                    current_class = Some((
                        obfuscated.to_owned(),
                        ClassMapping {
                            original_name: original.to_owned(),
                            file_name: None,
                            methods: Vec::new(),
                            fields: HashMap::new(),
                        },
                    ));
                }
                continue;
            }

            if let Some((_, class)) = current_class.as_mut() {
                parse_member_line(line.trim(), class);
            }
        }

        if let Some((obfuscated, class)) = current_class {
            classes.insert(obfuscated, class);
        }

        Ok(Self { classes })
    }

    fn parse_many<'a>(inputs: impl IntoIterator<Item = &'a str>) -> Result<Self, AppError> {
        let mut classes: HashMap<String, ClassMapping> = HashMap::new();

        for input in inputs {
            let mapping = Self::parse(input)?;
            for (obfuscated_name, class) in mapping.classes {
                match classes.entry(obfuscated_name) {
                    Entry::Occupied(mut occupied) => occupied.get_mut().merge(class),
                    Entry::Vacant(vacant) => {
                        vacant.insert(class);
                    }
                }
            }
        }

        Ok(ProguardMapping { classes })
    }

    fn retrace(&self, stacktrace: &str) -> String {
        let mut out = String::with_capacity(stacktrace.len());

        let mut first = true;
        for line in stacktrace.lines() {
            if !first {
                out.push('\n');
            }
            first = false;
            self.retrace_line_into(line, &mut out);
        }

        if stacktrace.ends_with('\n') {
            out.push('\n');
        }

        out
    }

    fn retrace_line_into(&self, line: &str, out: &mut String) {
        let trimmed = line.trim_start();
        let prefix_len = line.len() - trimmed.len();
        let prefix = &line[..prefix_len];

        if let Some(rest) = trimmed.strip_prefix("at ") {
            self.retrace_stack_frame(prefix, rest, out);
        } else {
            self.retrace_exception_line_into(line, out);
        }
    }

    fn retrace_stack_frame(&self, prefix: &str, rest: &str, out: &mut String) {
        let Some(paren_start) = rest.find('(') else {
            out.push_str(prefix);
            out.push_str("at ");
            out.push_str(rest);
            return;
        };

        let qualified = &rest[..paren_start];
        let location = &rest[paren_start..];
        let (class_prefix, qualified) = split_container_prefix(qualified);

        let Some(dot_pos) = qualified.rfind('.') else {
            out.push_str(prefix);
            out.push_str("at ");
            out.push_str(rest);
            return;
        };

        let obf_class = &qualified[..dot_pos];
        let obf_method = &qualified[dot_pos + 1..];

        let Some(class) = self.classes.get(obf_class) else {
            out.push_str(prefix);
            out.push_str("at ");
            out.push_str(rest);
            return;
        };

        let line_num = parse_stacktrace_line_number(location);
        let resolved_method = self.resolve_method(class, obf_method, line_num);
        let method_name = resolved_method
            .map(|m| m.original_name.as_str())
            .unwrap_or(obf_method);

        let source_file = class.file_name.as_deref().unwrap_or("Unknown Source");

        out.push_str(prefix);
        out.push_str("at ");
        out.push_str(class_prefix);
        out.push_str(&class.original_name);
        out.push('.');
        out.push_str(method_name);

        match line_num {
            Some(n) => {
                out.push('(');
                out.push_str(source_file);
                out.push(':');
                out.push_str(&n.to_string());
                out.push(')');
            }
            None => {
                out.push('(');
                out.push_str(source_file);
                out.push(')');
            }
        }
    }

    fn retrace_exception_line_into(&self, line: &str, out: &mut String) {
        let trimmed = line.trim_start();
        let prefix_len = line.len() - trimmed.len();
        let prefix = &line[..prefix_len];

        let (before_class, class_and_rest) = if let Some(rest) = trimmed.strip_prefix("Caused by: ")
        {
            ("Caused by: ", rest)
        } else {
            ("", trimmed)
        };

        let (class_part, suffix) = class_and_rest
            .split_once(": ")
            .map(|(c, m)| (c, Some(m)))
            .unwrap_or((class_and_rest, None));

        let (class_prefix, obf_class) = split_container_prefix(class_part);

        if let Some(class) = self.classes.get(obf_class) {
            out.push_str(prefix);
            out.push_str(before_class);
            out.push_str(class_prefix);
            out.push_str(&class.original_name);

            if let Some(message) = suffix {
                out.push_str(": ");
                out.push_str(message);
            }
        } else {
            out.push_str(line);
        }
    }

    fn resolve_method<'a>(
        &'a self,
        class: &'a ClassMapping,
        obf_method: &str,
        line: Option<u32>,
    ) -> Option<&'a MethodMapping> {
        if let Some(line_num) = line {
            class
                .methods
                .iter()
                .find(|m| {
                    m.obfuscated_name == obf_method
                        && matches!(
                            (m.start_line, m.end_line),
                            (Some(start), Some(end))
                                if line_num >= start && line_num <= end
                        )
                })
                .or_else(|| {
                    class
                        .methods
                        .iter()
                        .find(|m| m.obfuscated_name == obf_method)
                })
        } else {
            class
                .methods
                .iter()
                .find(|m| m.obfuscated_name == obf_method)
        }
    }

    #[cfg(test)]
    fn resolve(
        &self,
        class_name: &str,
        method_name: Option<&str>,
        line: Option<u32>,
    ) -> Result<OriginalPosition, AppError> {
        let class = self.classes.get(class_name).ok_or(AppError::NotFound)?;
        let resolved_method =
            method_name.and_then(|method| self.resolve_method(class, method, line));

        Ok(OriginalPosition {
            source: class.original_name.clone(),
            line: line.unwrap_or(0),
            column: 0,
            name: resolved_method.map(|m| m.original_name.clone()),
        })
    }
}

impl ClassMapping {
    fn merge(&mut self, other: ClassMapping) {
        if self.file_name.is_none() {
            self.file_name = other.file_name;
        }
        self.methods.extend(other.methods);
        self.fields.extend(other.fields);
    }
}

fn extract_file_name(line: &str) -> Option<String> {
    let json_start = line.find('{')?;
    let meta = serde_json::from_str::<serde_json::Value>(&line[json_start..]).ok()?;
    meta.get("fileName")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
}

fn parse_class_line(line: &str) -> Option<(&str, &str)> {
    let line = line.strip_suffix(':')?;
    let (original, obfuscated) = line.split_once(" -> ")?;
    Some((original.trim(), obfuscated.trim()))
}

fn split_container_prefix(qualified: &str) -> (&str, &str) {
    if let Some(slash_pos) = qualified.rfind('/') {
        (&qualified[..=slash_pos], &qualified[slash_pos + 1..])
    } else {
        ("", qualified)
    }
}

fn parse_stacktrace_line_number(location: &str) -> Option<u32> {
    location
        .trim_start_matches('(')
        .trim_end_matches(')')
        .rsplit_once(':')
        .and_then(|(_, num)| num.parse::<u32>().ok())
}

fn parse_member_line(line: &str, class: &mut ClassMapping) {
    let Some((original_part, obfuscated)) = line.rsplit_once(" -> ") else {
        return;
    };

    let obfuscated = obfuscated.trim();

    if let Some(method) = parse_method_with_lines(original_part) {
        class.methods.push(MethodMapping {
            original_name: method.name,
            obfuscated_name: obfuscated.to_owned(),
            start_line: Some(method.start_line),
            end_line: Some(method.end_line),
        });
        return;
    }

    if original_part.contains('(') {
        if let Some(method_name) = parse_method_name(original_part) {
            class.methods.push(MethodMapping {
                original_name: method_name,
                obfuscated_name: obfuscated.to_owned(),
                start_line: None,
                end_line: None,
            });
        }
        return;
    }

    if let Some(field_name) = original_part.split_whitespace().last() {
        class
            .fields
            .insert(obfuscated.to_owned(), field_name.to_owned());
    }
}

struct ParsedMethod {
    name: String,
    start_line: u32,
    end_line: u32,
}

fn parse_method_name(s: &str) -> Option<String> {
    let paren_pos = s.find('(')?;
    let before_paren = &s[..paren_pos];
    let method_name = before_paren
        .rsplit_once(' ')
        .map(|(_, name)| name)
        .unwrap_or(before_paren);
    Some(method_name.to_owned())
}

fn parse_method_with_lines(s: &str) -> Option<ParsedMethod> {
    let s = s.trim();
    let (start_str, rest) = s.split_once(':')?;
    let start_line = start_str.parse().ok()?;
    let (end_str, rest) = rest.split_once(':')?;
    let end_line = end_str.parse().ok()?;

    let paren_pos = rest.find('(')?;
    let before_paren = &rest[..paren_pos];
    let method_name = before_paren
        .rsplit_once(' ')
        .map(|(_, name)| name)
        .unwrap_or(before_paren);

    Some(ParsedMethod {
        name: method_name.to_owned(),
        start_line,
        end_line,
    })
}

pub async fn ingest(
    storage: &Storage,
    project_id: Uuid,
    build_id: &str,
    mappings: &[(String, String)],
) -> Result<(), AppError> {
    for (file_name, mapping) in mappings {
        let key = proguard_s3_key(project_id, build_id, file_name);
        storage.put(&key, mapping.as_bytes()).await?;
    }

    Ok(())
}

pub fn retrace_stacktrace<'a>(
    mapping_parts: impl IntoIterator<Item = &'a [u8]>,
    stacktrace: &str,
) -> Result<String, AppError> {
    let mut contents = Vec::new();

    for data in mapping_parts {
        let content = std::str::from_utf8(data)
            .map_err(|e| AppError::BadRequest(format!("invalid proguard mapping: {e}")))?;
        contents.push(content);
    }

    let mapping = ProguardMapping::parse_many(contents)?;
    Ok(mapping.retrace(stacktrace))
}

pub fn proguard_s3_prefix(project_id: Uuid, build_id: &str) -> String {
    s3_key(project_id, build_id, PROGUARD_DIR)
}

pub fn proguard_s3_key(project_id: Uuid, build_id: &str, file_name: &str) -> String {
    s3_key(project_id, build_id, &format!("{PROGUARD_DIR}/{file_name}"))
}

#[cfg(test)]
fn parse_test_mapping(input: &str) -> ProguardMapping {
    ProguardMapping::parse(input).unwrap()
}

#[cfg(test)]
fn parse_test_mappings<'a>(inputs: impl IntoIterator<Item = &'a str>) -> ProguardMapping {
    ProguardMapping::parse_many(inputs).unwrap()
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
        let mapping = parse_test_mapping(SAMPLE_MAPPING);
        assert!(mapping.classes.contains_key("a.a.a"));
        assert!(mapping.classes.contains_key("a.a.b"));

        let file_io = &mapping.classes["a.a.a"];
        assert_eq!(file_io.original_name, "core.file.FileIO");
        assert_eq!(file_io.file_name.as_deref(), Some("FileIO.java"));
    }

    #[test]
    fn parse_field_mappings() {
        let mapping = parse_test_mapping(SAMPLE_MAPPING);
        let file_io = &mapping.classes["a.a.a"];
        assert_eq!(file_io.fields.get("a"), Some(&"file".to_string()));
        assert_eq!(file_io.fields.get("b"), Some(&"charset".to_string()));
        assert_eq!(file_io.fields.get("d"), Some(&"loaded".to_string()));
    }

    #[test]
    fn parse_method_mappings() {
        let mapping = parse_test_mapping(SAMPLE_MAPPING);
        let file_io = &mapping.classes["a.a.a"];

        let init_methods: Vec<_> = file_io
            .methods
            .iter()
            .filter(|m| m.obfuscated_name == "<init>")
            .collect();
        assert_eq!(init_methods.len(), 2);
        assert_eq!(init_methods[0].start_line, Some(29));
        assert_eq!(init_methods[0].end_line, Some(33));
        assert_eq!(init_methods[1].start_line, Some(42));
        assert_eq!(init_methods[1].end_line, Some(43));
    }

    #[test]
    fn parse_method_mappings_without_line_numbers() {
        let mapping = parse_test_mapping(SAMPLE_MAPPING);
        let file_io = &mapping.classes["a.a.a"];

        let load_method = file_io
            .methods
            .iter()
            .find(|m| m.obfuscated_name == "b")
            .expect("load method should be retained");

        assert_eq!(load_method.original_name, "load");
        assert_eq!(load_method.start_line, None);
        assert_eq!(load_method.end_line, None);
    }

    #[test]
    fn resolve_class() {
        let mapping = parse_test_mapping(SAMPLE_MAPPING);
        let result = mapping.resolve("a.a.a", None, None).unwrap();
        assert_eq!(result.source, "core.file.FileIO");
    }

    #[test]
    fn resolve_method_with_line() {
        let mapping = parse_test_mapping(SAMPLE_MAPPING);
        let result = mapping.resolve("a.a.a", Some("c"), Some(92)).unwrap();
        assert_eq!(result.source, "core.file.FileIO");
        assert_eq!(result.name.as_deref(), Some("reload"));
    }

    #[test]
    fn resolve_method_without_line() {
        let mapping = parse_test_mapping(SAMPLE_MAPPING);
        let result = mapping.resolve("a.a.a", Some("a"), None).unwrap();
        assert_eq!(result.source, "core.file.FileIO");
        // Should find one of the methods named "a" (setRoot or getRoot)
        assert!(result.name.is_some());
    }

    #[test]
    fn resolve_unknown_class() {
        let mapping = parse_test_mapping(SAMPLE_MAPPING);
        let result = mapping.resolve("z.z.z", None, None);
        assert!(result.is_err());
    }

    #[test]
    fn retrace_stacktrace_full() {
        let mapping = parse_test_mapping(SAMPLE_MAPPING);
        let input = "\
java.lang.NullPointerException: something broke
\tat a.a.a.c(SourceFile:92)
\tat a.a.a.<init>(SourceFile:30)
\tat a.a.b.a_(SourceFile:26)";

        let output = mapping.retrace(input);
        assert_eq!(
            output,
            "\
java.lang.NullPointerException: something broke
\tat core.file.FileIO.reload(FileIO.java:92)
\tat core.file.FileIO.<init>(FileIO.java:30)
\tat core.file.Validatable.validate(Validatable.java:26)"
        );
    }

    #[test]
    fn retrace_preserves_unknown_lines() {
        let mapping = parse_test_mapping(SAMPLE_MAPPING);
        let input = "\
java.lang.RuntimeException: oops
\tat a.a.a.c(SourceFile:92)
\tat com.unknown.Foo.bar(Foo.java:10)
\t... 3 more";

        let output = mapping.retrace(input);
        assert_eq!(
            output,
            "\
java.lang.RuntimeException: oops
\tat core.file.FileIO.reload(FileIO.java:92)
\tat com.unknown.Foo.bar(Foo.java:10)
\t... 3 more"
        );
    }

    #[test]
    fn retrace_caused_by() {
        let mapping = parse_test_mapping(SAMPLE_MAPPING);
        let input = "Caused by: a.a.a: some message";
        let output = mapping.retrace(input);
        assert_eq!(output, "Caused by: core.file.FileIO: some message");
    }

    #[test]
    fn retrace_unknown_source() {
        let mapping = parse_test_mapping(SAMPLE_MAPPING);
        let input = "\tat a.a.a.c(Unknown Source)";
        let output = mapping.retrace(input);
        assert_eq!(output, "\tat core.file.FileIO.reload(FileIO.java)");
    }

    #[test]
    fn retrace_unknown_source_uses_method_without_line_numbers() {
        let mapping = parse_test_mapping(SAMPLE_MAPPING);
        let input = "\tat a.a.a.b(Unknown Source)";
        let output = mapping.retrace(input);
        assert_eq!(output, "\tat core.file.FileIO.load(FileIO.java)");
    }

    #[test]
    fn retrace_stacktrace_with_container_prefix() {
        let mapping = parse_test_mapping(SAMPLE_MAPPING);
        let input = "\tat tweaks-3.3.6-obfuscated.jar//a.a.a.c(SourceFile:92)";
        let output = mapping.retrace(input);
        assert_eq!(
            output,
            "\tat tweaks-3.3.6-obfuscated.jar//core.file.FileIO.reload(FileIO.java:92)"
        );
    }

    #[test]
    fn retrace_caused_by_with_container_prefix() {
        let mapping = parse_test_mapping(SAMPLE_MAPPING);
        let input = "Caused by: tweaks-3.3.6-obfuscated.jar//a.a.a: some message";
        let output = mapping.retrace(input);
        assert_eq!(
            output,
            "Caused by: tweaks-3.3.6-obfuscated.jar//core.file.FileIO: some message"
        );
    }

    #[test]
    fn parse_many_combines_split_mapping_files() {
        const PART_ONE: &str = r#"core.file.FileIO -> a.a.a:
# {"fileName":"FileIO.java","id":"sourceFile"}
    92:92:core.file.FileIO reload() -> c
"#;
        const PART_TWO: &str = r#"core.file.Validatable -> a.a.b:
# {"fileName":"Validatable.java","id":"sourceFile"}
    26:26:core.file.FileIO validate() -> a_
"#;

        let mapping = parse_test_mappings([PART_ONE, PART_TWO]);

        assert_eq!(mapping.classes["a.a.a"].original_name, "core.file.FileIO");
        assert_eq!(
            mapping.classes["a.a.b"].file_name.as_deref(),
            Some("Validatable.java")
        );
    }

    #[test]
    fn parse_many_merges_split_class_mappings() {
        const PART_ONE: &str = r#"core.file.FileIO -> a.a.a:
# {"fileName":"FileIO.java","id":"sourceFile"}
    92:92:core.file.FileIO reload() -> c
"#;
        const PART_TWO: &str = r#"core.file.FileIO -> a.a.a:
    java.lang.Object load() -> b
"#;

        let mapping = parse_test_mappings([PART_ONE, PART_TWO]);
        let file_io = &mapping.classes["a.a.a"];

        assert_eq!(file_io.file_name.as_deref(), Some("FileIO.java"));
        assert!(file_io.methods.iter().any(|m| m.obfuscated_name == "c"));
        assert!(
            file_io
                .methods
                .iter()
                .any(|m| m.obfuscated_name == "b" && m.original_name == "load")
        );
    }

    #[test]
    fn retrace_stacktrace_uses_all_mapping_parts() {
        const PART_ONE: &str = r#"core.file.FileIO -> a.a.a:
# {"fileName":"FileIO.java","id":"sourceFile"}
    92:92:core.file.FileIO reload() -> c
"#;
        const PART_TWO: &str = r#"core.file.Validatable -> a.a.b:
# {"fileName":"Validatable.java","id":"sourceFile"}
    26:26:core.file.FileIO validate() -> a_
"#;

        let input = "\
\tat a.a.a.c(SourceFile:92)
\tat a.a.b.a_(SourceFile:26)";

        let output = retrace_stacktrace([PART_ONE.as_bytes(), PART_TWO.as_bytes()], input).unwrap();

        assert_eq!(
            output,
            "\
\tat core.file.FileIO.reload(FileIO.java:92)
\tat core.file.Validatable.validate(Validatable.java:26)"
        );
    }

    #[test]
    fn proguard_s3_key_uses_uploaded_file_name() {
        let project_id = Uuid::nil();
        let key = proguard_s3_key(project_id, "build-1", "base.txt");

        assert_eq!(
            key,
            "00000000-0000-0000-0000-000000000000/build-1/proguard/base.txt"
        );
    }
}
