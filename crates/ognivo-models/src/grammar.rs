//! Grammars for llama.cpp (GBNF; docs/architecture.md §5.7): JSON values, and JSON Schema
//! turned into rules.
//!
//! The schemas this takes are those ognivo's tools declare: objects of named properties,
//! some required and no others (`additionalProperties: false`), each a string, an integer, a
//! number, a boolean, null, an enum, an array of one of these, or such an object; a list of
//! types is their union. A property is written in the order the schema declares it, as
//! llama.cpp's own converter has it, and an optional one may be left out. A schema outside
//! these becomes any JSON value: the tools check what comes.

use std::collections::HashSet;

use serde_json::Value;

/// JSON as llama.cpp's `grammars/json.gbnf` has it, with `\/` among the escapes, deeper
/// indents, and the digits of an exponent as JSON has them.
const JSON: &str = r#"value ::= object | array | string | number | ( "true" | "false" | "null" ) ws
object ::= "{" ws ( string ":" ws value ( "," ws string ":" ws value )* )? "}" ws
array ::= "[" ws ( value ( "," ws value )* )? "]" ws
string ::= "\"" ( [^"\\\x7F\x00-\x1F] | "\\" ( ["\\/bfnrt] | "u" [0-9a-fA-F]{4} ) )* "\"" ws
number ::= "-"? ( "0" | [1-9] [0-9]{0,15} ) ( "." [0-9]+ )? ( [eE] [-+]? [0-9]{1,16} )? ws
integer ::= "-"? ( "0" | [1-9] [0-9]{0,15} ) ws
boolean ::= ( "true" | "false" ) ws
ws ::= | " " | "\n" [ \t]{0,40}
"#;

/// The rules of a grammar being built; the JSON rules come with them.
#[derive(Debug, Default)]
pub struct Rules {
    rules: Vec<(String, String)>,
    names: HashSet<String>,
}

impl Rules {
    /// Adds `name ::= body` under a name of its own: `name`, made a rule name, and numbered
    /// when taken. The name it got.
    pub fn add(&mut self, name: &str, body: String) -> String {
        let base: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let mut name = base.clone();
        let mut n = 1;
        while is_json_rule(&name) || !self.names.insert(name.clone()) {
            n += 1;
            name = format!("{base}-{n}");
        }
        self.rules.push((name.clone(), body));
        name
    }

    /// What admits a value of `schema`: a rule's name, or a group of them.
    pub fn schema(&mut self, schema: &Value, name: &str) -> String {
        if let Some(values) = schema.get("enum").and_then(Value::as_array) {
            let literals: Vec<String> = values.iter().map(|v| literal(&v.to_string())).collect();
            if !literals.is_empty() {
                return format!("( {} ) ws", literals.join(" | "));
            }
        }
        match schema.get("type") {
            Some(Value::String(t)) => self.typed(t, schema, name),
            Some(Value::Array(types)) => {
                let alternatives: Vec<String> = types
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|t| self.typed(t, schema, name))
                    .collect();
                if alternatives.is_empty() {
                    "value".into()
                } else {
                    format!("( {} )", alternatives.join(" | "))
                }
            }
            _ => "value".into(),
        }
    }

    fn typed(&mut self, t: &str, schema: &Value, name: &str) -> String {
        match t {
            "string" => "string".into(),
            "integer" => "integer".into(),
            "number" => "number".into(),
            "boolean" => "boolean".into(),
            "null" => "\"null\" ws".into(),
            "array" => {
                let item = match schema.get("items") {
                    Some(items) => self.schema(items, &format!("{name}-item")),
                    None => "value".into(),
                };
                let body = format!("\"[\" ws ( {item} ( \",\" ws {item} )* )? \"]\" ws");
                self.add(name, body)
            }
            "object" => self.object(schema, name),
            _ => "value".into(),
        }
    }

    /// An object of the schema's properties in their order, the optional ones maybe left
    /// out, and no others. Rule `i-first` reads from property i on with nothing written yet,
    /// `i-after` with something before it, so a comma goes only between two.
    fn object(&mut self, schema: &Value, name: &str) -> String {
        let Some(props) = schema.get("properties").and_then(Value::as_object) else {
            return "object".into();
        };
        let required: HashSet<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let props: Vec<(&String, &Value)> = props.iter().collect();
        // from the last property back, so each rule names the ones after it
        let (mut first, mut after) = (String::new(), String::new());
        for (i, (key, value)) in props.iter().enumerate().rev() {
            let value = self.schema(value, &format!("{name}-{key}"));
            let this = format!(
                "{} ws \":\" ws {value}",
                literal(&Value::String((*key).clone()).to_string())
            );
            let (f, a) = if required.contains(key.as_str()) {
                (
                    format!("{this} {after}"),
                    format!("\",\" ws {this} {after}"),
                )
            } else if first.is_empty() {
                (format!("( {this} )?"), format!("( \",\" ws {this} )?"))
            } else {
                (
                    format!("{this} {after} | {first}"),
                    format!("( \",\" ws {this} )? {after}"),
                )
            };
            first = self.add(&format!("{name}-{i}-first"), f.trim_end().to_owned());
            // the first property never has one before it
            if i > 0 {
                after = self.add(&format!("{name}-{i}-after"), a.trim_end().to_owned());
            }
        }
        let body = format!("\"{{\" ws {first} \"}}\" ws");
        self.add(name, body)
    }

    /// The grammar: `root ::= root`, the rules, then JSON's.
    pub fn grammar(&self, root: &str) -> String {
        let mut out = format!("root ::= {root}\n");
        for (name, body) in &self.rules {
            out.push_str(&format!("{name} ::= {body}\n"));
        }
        out.push_str(JSON);
        out
    }
}

fn is_json_rule(name: &str) -> bool {
    [
        "root", "value", "object", "array", "string", "number", "integer", "boolean", "ws",
    ]
    .contains(&name)
}

/// `text` as a GBNF literal.
pub fn literal(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_object_keeps_its_properties_in_order_and_no_others() {
        let mut r = Rules::default();
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "offset": {"type": "integer"},
                "limit": {"type": "integer"}
            },
            "required": ["path"],
            "additionalProperties": false
        });
        let root = r.schema(&schema, "read");
        assert_eq!(root, "read");
        let g = r.grammar(&root);
        // path is required, offset and limit may each be left out
        assert!(g.contains("read-0-first ::= \"\\\"path\\\"\" ws \":\" ws string read-1-after\n"));
        assert!(g.contains(
            "read-1-after ::= ( \",\" ws \"\\\"offset\\\"\" ws \":\" ws integer )? read-2-after\n"
        ));
        assert!(
            g.contains("read-2-after ::= ( \",\" ws \"\\\"limit\\\"\" ws \":\" ws integer )?\n")
        );
        assert!(g.contains("read ::= \"{\" ws read-0-first \"}\" ws\n"));
    }

    #[test]
    fn values_take_their_types() {
        let mut r = Rules::default();
        assert_eq!(r.schema(&json!({"type": "boolean"}), "x"), "boolean");
        assert_eq!(
            r.schema(&json!({"enum": ["fast", "slow"]}), "x"),
            "( \"\\\"fast\\\"\" | \"\\\"slow\\\"\" ) ws"
        );
        assert_eq!(
            r.schema(&json!({"type": ["string", "null"]}), "x"),
            "( string | \"null\" ws )"
        );
        assert_eq!(
            r.schema(
                &json!({"type": "array", "items": {"type": "string"}}),
                "cmd"
            ),
            "cmd"
        );
        // what it does not know is any JSON value
        assert_eq!(r.schema(&json!({"oneOf": []}), "x"), "value");
        // names stay apart from each other and from JSON's
        assert_eq!(r.add("string", "\"a\"".into()), "string-2");
        assert_eq!(r.add("cmd", "\"b\"".into()), "cmd-2");
    }
}
