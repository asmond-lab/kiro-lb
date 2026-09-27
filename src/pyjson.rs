//! Python `json.dumps` defaults: `", "` / `": "` separators and `ensure_ascii`.
//! Tool-call arguments were re-serialized that way by the reference gateway, and
//! clients diff them, so the byte form is kept.

use serde_json::Value;
use std::fmt::Write;

pub fn dumps(v: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, v, false);
    out
}

pub fn dumps_sorted(v: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, v, true);
    out
}

fn write_value(out: &mut String, v: &Value, sort: bool) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => write_number(out, n),
        Value::String(s) => write_string(out, s),
        Value::Array(a) => {
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_value(out, item, sort);
            }
            out.push(']');
        }
        Value::Object(o) => {
            out.push('{');
            let mut entries: Vec<(&String, &Value)> = o.iter().collect();
            if sort {
                entries.sort_by(|a, b| a.0.cmp(b.0));
            }
            for (i, (k, val)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_string(out, k);
                out.push_str(": ");
                write_value(out, val, sort);
            }
            out.push('}');
        }
    }
}

fn write_number(out: &mut String, n: &serde_json::Number) {
    if let Some(f) = n.as_f64().filter(|_| n.is_f64()) {
        if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e16 {
            let _ = write!(out, "{f:.1}");
        } else if f.is_finite() {
            let s = format!("{f:?}");
            out.push_str(&s);
        } else if f.is_nan() {
            out.push_str("NaN");
        } else {
            out.push_str(if f > 0.0 { "Infinity" } else { "-Infinity" });
        }
    } else {
        out.push_str(&n.to_string());
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_python_defaults() {
        let v: Value =
            serde_json::from_str(r#"{"a":1,"b":[1.5,"한"],"c":{"d":null,"e":2.0}}"#).unwrap();
        let expected = format!(
            "{{\"a\": 1, \"b\": [1.5, \"{}ud55c\"], \"c\": {{\"d\": null, \"e\": 2.0}}}}",
            '\\'
        );
        assert_eq!(dumps(&v), expected);
    }
}
