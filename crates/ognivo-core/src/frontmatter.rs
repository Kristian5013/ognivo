//! Markdown files with YAML frontmatter, read as Claude Code reads its skills, commands and
//! subagents: there is frontmatter only when `---` is the file's first line, and it runs to
//! the next `---` line. Field names count exactly as written and unknown ones are ignored.
//! Frontmatter that is not YAML leaves the file its body with no fields.

use yaml_rust2::{Yaml, YamlLoader};

/// A file split into its fields and its body.
#[derive(Debug, Clone, Default)]
pub struct Document {
    fields: Option<yaml_rust2::yaml::Hash>,
    pub body: String,
    /// Why the frontmatter was not read, when it was not.
    pub problem: Option<String>,
}

impl Document {
    pub fn parse(text: &str) -> Self {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut lines = text.split_inclusive('\n');
        let Some(first) = lines.next().filter(|l| l.trim_end() == "---") else {
            return Self {
                body: text.to_owned(),
                ..Self::default()
            };
        };
        let start = first.len();
        let mut at = start;
        let mut end = None;
        for line in lines {
            if line.trim_end() == "---" {
                end = Some((at, at + line.len()));
                break;
            }
            at += line.len();
        }
        let Some((yaml_end, body_start)) = end else {
            return Self {
                body: text.to_owned(),
                problem: Some("its frontmatter has no closing `---` line".into()),
                ..Self::default()
            };
        };
        let body = text[body_start..].to_owned();
        let (fields, problem) = match YamlLoader::load_from_str(&text[start..yaml_end]) {
            Ok(docs) => match docs.into_iter().next() {
                Some(Yaml::Hash(h)) => (Some(h), None),
                None | Some(Yaml::Null) => (None, None),
                Some(_) => (None, Some("its frontmatter is not a mapping".into())),
            },
            Err(e) => (None, Some(format!("its frontmatter is not YAML: {e}"))),
        };
        Self {
            fields,
            body,
            problem,
        }
    }

    fn get(&self, key: &str) -> Option<&Yaml> {
        self.fields.as_ref()?.get(&Yaml::String(key.to_owned()))
    }

    pub fn has(&self, key: &str) -> bool {
        self.get(key).is_some_and(|v| !matches!(v, Yaml::Null))
    }

    /// A field's value as text, trimmed; `None` when it is missing, empty or not a scalar.
    pub fn text(&self, key: &str) -> Option<String> {
        scalar(self.get(key)?).filter(|s| !s.is_empty())
    }

    /// A yes-or-no field: true/false, yes/no, on/off or 1/0 in any case.
    pub fn flag(&self, key: &str) -> Option<bool> {
        match self.get(key)? {
            Yaml::Boolean(b) => Some(*b),
            Yaml::Integer(1) => Some(true),
            Yaml::Integer(0) => Some(false),
            Yaml::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "on" | "1" => Some(true),
                "false" | "no" | "off" | "0" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }

    /// A list field: a YAML sequence, or a text of items between commas or blanks outside
    /// parentheses, so that `Bash(git add *), Read` is two items.
    pub fn list(&self, key: &str) -> Vec<String> {
        match self.get(key) {
            Some(Yaml::Array(items)) => items
                .iter()
                .filter_map(scalar)
                .filter(|s| !s.is_empty())
                .collect(),
            Some(v) => scalar(v).map(|s| split_list(&s)).unwrap_or_default(),
            None => Vec::new(),
        }
    }
}

fn scalar(v: &Yaml) -> Option<String> {
    match v {
        Yaml::String(s) | Yaml::Real(s) => Some(s.trim().to_owned()),
        Yaml::Integer(i) => Some(i.to_string()),
        Yaml::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Items between commas or blanks that are not inside parentheses.
pub fn split_list(text: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut item = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '(' => {
                depth += 1;
                item.push(c);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                item.push(c);
            }
            c if depth == 0 && (c == ',' || c.is_whitespace()) => {
                if !item.is_empty() {
                    items.push(std::mem::take(&mut item));
                }
            }
            c => item.push(c),
        }
    }
    if !item.is_empty() {
        items.push(item);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_come_only_from_a_first_line_of_dashes() {
        let d = Document::parse(
            "---\nname: pdf\ndescription: >-\n  Reads PDFs,\n  fills forms.\ndisable-model-invocation: yes\nallowed-tools: Bash(git add *), Read Grep\narguments: [issue, branch]\n---\n# PDF\n\nBody.\n",
        );
        assert_eq!(d.problem, None);
        assert_eq!(d.text("name").as_deref(), Some("pdf"));
        assert_eq!(
            d.text("description").as_deref(),
            Some("Reads PDFs, fills forms.")
        );
        assert_eq!(d.flag("disable-model-invocation"), Some(true));
        assert_eq!(d.flag("user-invocable"), None);
        assert_eq!(d.list("allowed-tools"), ["Bash(git add *)", "Read", "Grep"]);
        assert_eq!(d.list("arguments"), ["issue", "branch"]);
        assert_eq!(d.body, "# PDF\n\nBody.\n");

        let plain = Document::parse("Just a body.\n---\nname: no\n---\n");
        assert!(!plain.has("name"));
        assert_eq!(plain.body, "Just a body.\n---\nname: no\n---\n");

        let bad = Document::parse("---\nname: [unclosed\n---\r\nBody\r\n");
        assert!(bad.problem.as_ref().unwrap().contains("not YAML"));
        assert!(!bad.has("name"));
        assert_eq!(bad.body, "Body\r\n");

        let open = Document::parse("---\nname: x\n");
        assert!(open.problem.unwrap().contains("closing"));
    }
}
