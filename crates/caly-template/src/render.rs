//! Minimal `{{ dotted.path }}` template renderer.
//!
//! A deliberate, strict subset of the Tera syntax the worker previously
//! accepted via `tera::Tera::one_off` (autoescape off). Supporting only dotted
//! variable lookup removes the full template engine — and its pest parser,
//! chrono/chrono-tz timezone database, rand, slug and glob dependencies — from
//! both shipped binaries.
//!
//! Supported syntax:
//!
//! - `{{ path }}` with any amount of ASCII whitespace inside the braces.
//! - `path` is a `.`-separated chain of JSON object keys; each segment must be
//!   non-empty ASCII alphanumeric or `_`. Every hop must resolve: objects are
//!   traversed, the final value is rendered.
//! - Renderable values: strings verbatim, numbers in their JSON form,
//!   booleans as `true`/`false`. Null, arrays and objects are errors.
//!
//! Everything else — filters (`{{ a | upper }}`), expressions, whitespace
//! control (`{{- x }}`), `{% ... %}` blocks and `{# ... #}` comments — is
//! rejected as [`TemplateRenderError::UnsupportedExpression`] or
//! [`TemplateRenderError::UnsupportedTag`] so a template can never render
//! half-expanded by accident.
//!
//! Output is hard-capped at [`MAX_TEMPLATE_OUTPUT_BYTES`] *while rendering*;
//! exceeding the cap is an error. The worker applies the same bound again when
//! framing the response, but failing during the render avoids building an
//! oversized intermediate string (a 4 MiB source could otherwise fan a 16 MiB
//! context value out to many gigabytes).

use serde_json::{Map, Value};

use super::MAX_TEMPLATE_OUTPUT_BYTES;

/// Rendering failure with a human-readable diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TemplateRenderError {
    /// `{{` without a matching `}}` before the end of the source.
    UnterminatedVariable {
        /// Byte offset of the opening `{{`.
        offset: usize,
    },
    /// The text inside `{{ }}` is not a supported dotted variable path.
    UnsupportedExpression {
        /// The rejected expression, whitespace-trimmed.
        expression: String,
    },
    /// A `{%` block or `{#` comment tag; unsupported by design.
    UnsupportedTag {
        /// Byte offset of the opening `{`.
        offset: usize,
    },
    /// The dotted path did not resolve in the context object.
    UnknownVariable {
        /// The full path that failed to resolve.
        path: String,
    },
    /// The resolved value is not a string, number, or boolean.
    UnsupportedValue {
        /// The full path that resolved to a non-scalar value.
        path: String,
    },
    /// The context root is not a JSON object.
    ContextNotObject,
    /// Rendered output would exceed [`MAX_TEMPLATE_OUTPUT_BYTES`].
    OutputTooLarge,
}

impl core::fmt::Display for TemplateRenderError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnterminatedVariable { offset } => {
                write!(
                    f,
                    "template source contains an unterminated `{{{{` at byte {offset}"
                )
            }
            Self::UnsupportedExpression { expression } => write!(
                f,
                "unsupported template expression `{expression}` (only dotted variable paths are supported)"
            ),
            Self::UnsupportedTag { offset } => write!(
                f,
                "unsupported template tag at byte {offset} (`{{% ... %}}` blocks and `{{# ... #}}` comments are not supported)"
            ),
            Self::UnknownVariable { path } => {
                write!(f, "template variable `{path}` not found in context")
            }
            Self::UnsupportedValue { path } => {
                write!(
                    f,
                    "template variable `{path}` is not a string, number, or boolean"
                )
            }
            Self::ContextNotObject => write!(f, "template context must be a JSON object"),
            Self::OutputTooLarge => write!(f, "template output exceeds configured limit"),
        }
    }
}

impl std::error::Error for TemplateRenderError {}

/// Renders `source`, substituting every `{{ dotted.path }}` from `context`.
///
/// # Errors
///
/// Returns [`TemplateRenderError`] as documented on each variant: syntax the
/// renderer does not support, unresolvable paths, non-scalar values, a
/// non-object context, or output that exceeds the bounded capacity.
pub fn render_template(source: &str, context: &Value) -> Result<String, TemplateRenderError> {
    let Value::Object(root) = context else {
        return Err(TemplateRenderError::ContextNotObject);
    };
    let bytes = source.as_bytes();
    let mut output = String::with_capacity(source.len().min(MAX_TEMPLATE_OUTPUT_BYTES));
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor] == b'{' {
            match bytes.get(cursor + 1).copied() {
                Some(b'{') => {
                    let expression_end = source[cursor + 2..]
                        .find("}}")
                        .map(|relative| cursor + 2 + relative)
                        .ok_or(TemplateRenderError::UnterminatedVariable { offset: cursor })?;
                    let expression = source[cursor + 2..expression_end].trim();
                    let rendered = resolve_path(root, expression)?;
                    push_bounded(&mut output, &rendered)?;
                    cursor = expression_end + 2;
                    continue;
                }
                Some(b'%' | b'#') => {
                    return Err(TemplateRenderError::UnsupportedTag { offset: cursor });
                }
                _ => {}
            }
        }
        let width = utf8_char_width(bytes[cursor]);
        push_bounded(&mut output, &source[cursor..cursor + width])?;
        cursor += width;
    }
    Ok(output)
}

/// Resolves a whitespace-trimmed dotted path against the context root and
/// renders the scalar it lands on.
fn resolve_path(
    root: &Map<String, Value>,
    expression: &str,
) -> Result<String, TemplateRenderError> {
    if expression.is_empty() {
        return Err(unsupported_expression(expression));
    }
    let mut map = root;
    let mut segments = expression.split('.').peekable();
    let value = loop {
        let Some(segment) = segments.next() else {
            return Err(unsupported_expression(expression));
        };
        if !is_path_segment(segment) {
            return Err(unsupported_expression(expression));
        }
        match map.get(segment) {
            Some(value) if segments.peek().is_none() => break value,
            Some(Value::Object(nested)) => map = nested,
            Some(_) | None => {
                return Err(TemplateRenderError::UnknownVariable {
                    path: expression.to_owned(),
                });
            }
        }
    };
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Number(number) => Ok(number.to_string()),
        Value::Bool(flag) => Ok(flag.to_string()),
        _ => Err(TemplateRenderError::UnsupportedValue {
            path: expression.to_owned(),
        }),
    }
}

/// Byte width of a UTF-8 character given its leading byte. `source` is a
/// `&str`, hence valid UTF-8, so the leading byte determines the width.
fn utf8_char_width(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// A path segment is non-empty ASCII alphanumerics and underscores. Anything
/// else (`|`, `-`, quotes, spaces, brackets) means the template is using
/// syntax this renderer deliberately does not implement.
fn is_path_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn unsupported_expression(expression: &str) -> TemplateRenderError {
    TemplateRenderError::UnsupportedExpression {
        expression: expression.to_owned(),
    }
}

/// Appends one fragment, refusing to grow past the bounded output capacity.
/// (`output.len()` never exceeds the cap by induction, and a fragment is at
/// most one bounded context value or bounded source span, so the addition
/// cannot overflow.)
fn push_bounded(output: &mut String, fragment: &str) -> Result<(), TemplateRenderError> {
    if output.len() + fragment.len() > MAX_TEMPLATE_OUTPUT_BYTES {
        return Err(TemplateRenderError::OutputTooLarge);
    }
    output.push_str(fragment);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(source: &str, context: &str) -> Result<String, String> {
        let context: Value = serde_json::from_str(context).map_err(|error| error.to_string())?;
        render_template(source, &context).map_err(|error| error.to_string())
    }

    #[test]
    fn substitutes_strings_and_numbers_like_the_worker_e2e() -> Result<(), String> {
        let output = render(
            "node: {{ name }} / {{ latency }} ms",
            r#"{"name":"hk-edge-01","latency":28}"#,
        )?;
        assert_eq!(output, "node: hk-edge-01 / 28 ms");
        Ok(())
    }

    #[test]
    fn resolves_dotted_paths_and_booleans() -> Result<(), String> {
        let output = render(
            "{{ user.name }}:{{ user.active }}",
            r#"{"user":{"name":"caly","active":true}}"#,
        )?;
        assert_eq!(output, "caly:true");
        Ok(())
    }

    #[test]
    fn tolerates_whitespace_and_plain_brace_text() -> Result<(), String> {
        let output = render("a {{   name  }} b } c { d", r#"{"name":"x"}"#)?;
        assert_eq!(output, "a x b } c { d");
        Ok(())
    }

    #[test]
    fn missing_variable_is_an_unknown_variable_error() -> Result<(), String> {
        let context: Value = serde_json::from_str("{}").map_err(|e| e.to_string())?;
        let result = render_template("{{ bad_syntax }}", &context);
        assert_eq!(
            result,
            Err(TemplateRenderError::UnknownVariable {
                path: "bad_syntax".to_owned()
            })
        );
        Ok(())
    }

    #[test]
    fn scalar_with_deeper_path_is_an_unknown_variable_error() -> Result<(), String> {
        let context: Value =
            serde_json::from_str(r#"{"a":"scalar"}"#).map_err(|e| e.to_string())?;
        let result = render_template("{{ a.b }}", &context);
        assert_eq!(
            result,
            Err(TemplateRenderError::UnknownVariable {
                path: "a.b".to_owned()
            })
        );
        Ok(())
    }

    #[test]
    fn filters_and_expressions_are_rejected() -> Result<(), String> {
        for source in ["{{ a|upper }}", "{{- a }}", "{{ }}", "{{ a + 1 }}"] {
            let context: Value = serde_json::from_str(r#"{"a":"x"}"#).map_err(|e| e.to_string())?;
            let result = render_template(source, &context);
            assert!(
                matches!(
                    result,
                    Err(TemplateRenderError::UnsupportedExpression { .. })
                ),
                "{source} must be rejected as an unsupported expression"
            );
        }
        Ok(())
    }

    #[test]
    fn blocks_and_comments_are_rejected() -> Result<(), String> {
        for source in ["{% if a %}x{% endif %}", "{# comment #}"] {
            let context: Value = serde_json::from_str("{}").map_err(|e| e.to_string())?;
            let result = render_template(source, &context);
            assert!(
                matches!(result, Err(TemplateRenderError::UnsupportedTag { .. })),
                "{source} must be rejected as an unsupported tag"
            );
        }
        Ok(())
    }

    #[test]
    fn unterminated_variable_is_an_error() -> Result<(), String> {
        let context: Value = serde_json::from_str("{}").map_err(|e| e.to_string())?;
        let result = render_template("prefix {{ name", &context);
        assert_eq!(
            result,
            Err(TemplateRenderError::UnterminatedVariable { offset: 7 })
        );
        Ok(())
    }

    #[test]
    fn non_scalar_values_are_rejected() -> Result<(), String> {
        let context: Value = serde_json::from_str(r#"{"nothing":null,"list":[1],"obj":{}}"#)
            .map_err(|e| e.to_string())?;
        for path in ["nothing", "list", "obj"] {
            let result = render_template(&format!("{{{{ {path} }}}}"), &context);
            assert!(
                matches!(result, Err(TemplateRenderError::UnsupportedValue { .. })),
                "{path} must be rejected as a non-scalar value"
            );
        }
        Ok(())
    }

    #[test]
    fn non_object_context_is_rejected() -> Result<(), String> {
        let context: Value = serde_json::from_str("[1,2,3]").map_err(|e| e.to_string())?;
        let result = render_template("{{ a }}", &context);
        assert_eq!(result, Err(TemplateRenderError::ContextNotObject));
        Ok(())
    }

    #[test]
    fn output_is_capped_while_rendering() -> Result<(), String> {
        let filler = "x".repeat(1_024);
        let context: Value =
            serde_json::from_str(&format!(r#"{{"v":"{filler}"}}"#)).map_err(|e| e.to_string())?;
        // 4 MiB of `{{ v }}` repeats would fan the 1 KiB value out to ~600 MiB
        // without the mid-render cap.
        let source = "{{ v }}".repeat(600_000);
        let result = render_template(&source, &context);
        assert_eq!(result, Err(TemplateRenderError::OutputTooLarge));
        Ok(())
    }
}
