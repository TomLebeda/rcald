//! Minimal iCalendar (RFC 5545) reader: unfolds content lines and builds a component tree.

#[derive(Debug, Default)]
pub struct Component {
    pub name: String,
    pub props: Vec<Property>,
    pub children: Vec<Component>,
}

#[derive(Debug)]
pub struct Property {
    pub name: String,
    pub params: Vec<(String, String)>,
    pub value: String,
}

impl Component {
    pub fn prop(&self, name: &str) -> Option<&Property> {
        self.props.iter().find(|p| p.name == name)
    }

    pub fn props<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Property> {
        self.props.iter().filter(move |p| p.name == name)
    }

    pub fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Component> {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// Unescaped text value of a property.
    pub fn text(&self, name: &str) -> Option<String> {
        self.prop(name).map(|p| unescape(&p.value))
    }
}

impl Property {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// Parses an iCalendar document into a synthetic root component holding the top-level components.
pub fn parse(text: &str) -> anyhow::Result<Component> {
    let mut stack = vec![Component::default()];
    for line in unfold(text) {
        let Some(prop) = parse_line(&line) else { continue };
        match prop.name.as_str() {
            "BEGIN" => stack.push(Component { name: prop.value.to_ascii_uppercase(), ..Default::default() }),
            "END" if stack.len() > 1 => {
                let done = stack.pop().unwrap();
                stack.last_mut().unwrap().children.push(done);
            }
            "END" => anyhow::bail!("unbalanced END:{}", prop.value),
            _ => stack.last_mut().unwrap().props.push(prop),
        }
    }
    anyhow::ensure!(stack.len() == 1, "unterminated component {}", stack.last().unwrap().name);
    Ok(stack.pop().unwrap())
}

fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.lines() {
        match (raw.strip_prefix([' ', '\t']), lines.last_mut()) {
            (Some(rest), Some(last)) => last.push_str(rest),
            _ if !raw.trim().is_empty() => lines.push(raw.to_owned()),
            _ => {}
        }
    }
    lines
}

/// `NAME;PARAM=a;PARAM2="b:c":value` → Property. Colons and semicolons inside quotes are literal.
fn parse_line(line: &str) -> Option<Property> {
    let mut parts = Vec::new();
    let (mut start, mut quoted) = (0, false);
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ';' if !quoted => {
                parts.push(&line[start..i]);
                start = i + 1;
            }
            ':' if !quoted => {
                parts.push(&line[start..i]);
                let mut parts = parts.into_iter();
                let name = parts.next()?.to_ascii_uppercase();
                let params = parts
                    .filter_map(|p| p.split_once('='))
                    .map(|(k, v)| (k.to_ascii_uppercase(), v.trim_matches('"').to_owned()))
                    .collect();
                return Some(Property { name, params, value: line[i + 1..].to_owned() });
            }
            _ => {}
        }
    }
    None
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_components_with_folding_and_params() {
        let root = parse(
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:Long\r\n  title\\, escaped\r\n\
             DTSTART;TZID=\"Europe/Prague\":20261014T090000\r\nBEGIN:VALARM\r\nTRIGGER:-PT15M\r\n\
             END:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        )
        .unwrap();
        let event = root.children("VCALENDAR").next().unwrap().children("VEVENT").next().unwrap();
        assert_eq!(event.text("SUMMARY").unwrap(), "Long title, escaped");
        let start = event.prop("DTSTART").unwrap();
        assert_eq!(start.param("TZID"), Some("Europe/Prague"));
        assert_eq!(start.value, "20261014T090000");
        assert_eq!(event.children("VALARM").count(), 1);
    }
}
