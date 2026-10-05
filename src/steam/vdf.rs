//! Valve's text `KeyValues` format, used by Steam's `.vdf` and `.acf` files:
//! `"key" "value"` pairs and `"key" { … }` blocks, with `//` comments.

#[derive(Debug, PartialEq)]
pub enum Value {
    Text(String),
    Map(Map),
}

/// Keys in file order. Steam is not consistent about their case, so lookups ignore it.
#[derive(Debug, Default, PartialEq)]
pub struct Map(pub Vec<(String, Value)>);

impl Map {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    }

    pub fn text(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Value::Text(text)) => Some(text),
            _ => None,
        }
    }

    pub fn map(&self, key: &str) -> Option<&Map> {
        match self.get(key) {
            Some(Value::Map(map)) => Some(map),
            _ => None,
        }
    }

    /// The nested blocks, with their keys.
    pub fn maps(&self) -> impl Iterator<Item = (&str, &Map)> {
        self.0.iter().filter_map(|(k, v)| match v {
            Value::Map(map) => Some((k.as_str(), map)),
            Value::Text(_) => None,
        })
    }
}

pub fn parse(text: &str) -> Result<Map, String> {
    let mut tokens = Tokens {
        chars: text.chars().peekable(),
    };
    let map = block(&mut tokens, false)?;
    Ok(map)
}

#[derive(Debug, PartialEq)]
enum Token {
    Text(String),
    Open,
    Close,
}

struct Tokens<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl Tokens<'_> {
    fn next(&mut self) -> Result<Option<Token>, String> {
        loop {
            match self.chars.peek() {
                None => return Ok(None),
                Some(c) if c.is_whitespace() => {
                    self.chars.next();
                }
                Some('/') => {
                    self.chars.next();
                    if self.chars.next() != Some('/') {
                        return Err("a single '/' outside quotes".into());
                    }
                    while self.chars.next().is_some_and(|c| c != '\n') {}
                }
                Some('{') => {
                    self.chars.next();
                    return Ok(Some(Token::Open));
                }
                Some('}') => {
                    self.chars.next();
                    return Ok(Some(Token::Close));
                }
                Some('"') => {
                    self.chars.next();
                    let mut text = String::new();
                    loop {
                        match self.chars.next() {
                            None => return Err("unterminated quoted text".into()),
                            Some('"') => return Ok(Some(Token::Text(text))),
                            Some('\\') => match self.chars.next() {
                                Some('n') => text.push('\n'),
                                Some('t') => text.push('\t'),
                                Some(c) => text.push(c),
                                None => return Err("unterminated quoted text".into()),
                            },
                            Some(c) => text.push(c),
                        }
                    }
                }
                Some(_) => {
                    let mut text = String::new();
                    while let Some(&c) = self.chars.peek() {
                        if c.is_whitespace() || matches!(c, '{' | '}' | '"') {
                            break;
                        }
                        text.push(c);
                        self.chars.next();
                    }
                    return Ok(Some(Token::Text(text)));
                }
            }
        }
    }
}

fn block(tokens: &mut Tokens<'_>, nested: bool) -> Result<Map, String> {
    let mut map = Map::default();
    loop {
        let key = match tokens.next()? {
            Some(Token::Text(key)) => key,
            Some(Token::Close) if nested => return Ok(map),
            None if !nested => return Ok(map),
            Some(Token::Close) => return Err("unexpected '}'".into()),
            Some(Token::Open) => return Err("a block without a key".into()),
            None => return Err("missing '}'".into()),
        };
        let value = match tokens.next()? {
            Some(Token::Text(text)) => Value::Text(text),
            Some(Token::Open) => Value::Map(block(tokens, true)?),
            Some(Token::Close) | None => return Err(format!("no value for {key:?}")),
        };
        map.0.push((key, value));
    }
}

#[cfg(test)]
mod tests {
    use super::{Value, parse};

    #[test]
    fn reads_nested_blocks_with_escapes_and_comments() {
        let map = parse(
            "\"AppState\"\n{\n\t\"appid\"\t\t\"1364780\"\n\t// a comment\n\t\"name\"\t\t\"Street \\\"Fighter\\\" 6\"\n\t\"path\"\t\t\"C:\\\\Steam\"\n\t\"InstalledDepots\"\n\t{\n\t\t\"1\" { \"size\" \"5\" }\n\t}\n}\n",
        )
        .unwrap();
        let app = map.map("appstate").unwrap();
        assert_eq!(app.text("AppID"), Some("1364780"));
        assert_eq!(app.text("name"), Some("Street \"Fighter\" 6"));
        assert_eq!(app.text("path"), Some("C:\\Steam"));
        let depots: Vec<_> = app.map("InstalledDepots").unwrap().maps().collect();
        assert_eq!(depots.len(), 1);
        assert_eq!(depots[0].0, "1");
        assert_eq!(depots[0].1.get("size"), Some(&Value::Text("5".into())));
    }

    #[test]
    fn accepts_unquoted_tokens_and_rejects_broken_files() {
        let map = parse("root { key value }").unwrap();
        assert_eq!(map.map("root").unwrap().text("key"), Some("value"));
        assert!(parse("\"a\" { \"b\" \"c\"").is_err());
        assert!(parse("\"a\" \"b\" }").is_err());
        assert!(parse("\"a\"").is_err());
        assert!(parse("\"a\" \"unterminated").is_err());
    }
}
