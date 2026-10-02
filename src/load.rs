//! Reading a declaration file into the typed model (ADR 0001, ADR 0006).
//!
//! Both formats are first read into a format-independent tree of `Node`s,
//! and the model is then read from that tree. Building the tree rejects
//! input that a parser would otherwise accept silently (duplicate keys,
//! non-string keys and `null`), and reading the model from the tree accepts
//! only a mapping where the model expects a struct.

use std::fmt;
use std::path::Path;

use serde::de::value::{Error, MapDeserializer, SeqDeserializer};
use serde::de::{
    self, Deserialize, Deserializer, Expected, IntoDeserializer, MapAccess, SeqAccess, Unexpected,
    Visitor,
};

use crate::model::Declaration;

#[derive(Debug)]
pub enum LoadError {
    UnsupportedExtension,
    Read(std::io::Error),
    Empty,
    Syntax(String),
    Schema(String),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::UnsupportedExtension => {
                f.write_str("unsupported file extension; expected .yaml, .yml or .json")
            }
            LoadError::Read(e) => write!(f, "cannot read file: {e}"),
            LoadError::Empty => f.write_str("the declaration is empty"),
            LoadError::Syntax(e) => write!(f, "{e}"),
            LoadError::Schema(e) => write!(f, "{e}"),
        }
    }
}

enum Format {
    Yaml,
    Json,
}

fn format_of(path: &Path) -> Option<Format> {
    match path.extension()?.to_str()? {
        "yaml" | "yml" => Some(Format::Yaml),
        "json" => Some(Format::Json),
        _ => None,
    }
}

pub fn load(path: &Path) -> Result<Declaration, LoadError> {
    let format = format_of(path).ok_or(LoadError::UnsupportedExtension)?;
    let text = std::fs::read_to_string(path).map_err(LoadError::Read)?;
    let tree = tree(&text, format)
        .map_err(LoadError::Syntax)?
        .ok_or(LoadError::Empty)?;
    serde_path_to_error::deserialize(tree).map_err(|e| {
        let path = e.path().to_string();
        let inner = e.into_inner();
        LoadError::Schema(if path == "." {
            inner.to_string()
        } else {
            format!("{path}: {inner}")
        })
    })
}

/// Reads the document into a tree, or `None` for an empty document.
fn tree(text: &str, format: Format) -> Result<Option<Node>, String> {
    match format {
        Format::Yaml => serde_norway::from_str(text).map_err(|e| e.to_string()),
        Format::Json => serde_json::from_str(text).map_err(|e| e.to_string()),
    }
}

/// A value of the declaration, independent of the file format.
#[derive(Debug, PartialEq)]
enum Node {
    Bool(bool),
    Signed(i64),
    Unsigned(u64),
    Float(f64),
    String(String),
    Seq(Vec<Node>),
    /// Entries in document order, with unique keys.
    Map(Vec<(String, Node)>),
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(NodeVisitor)
    }
}

struct NodeVisitor;

impl<'de> Visitor<'de> for NodeVisitor {
    type Value = Node;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a string, number, boolean, sequence or mapping")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Node, E> {
        Ok(Node::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Node, E> {
        Ok(Node::Signed(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Node, E> {
        Ok(Node::Unsigned(v))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Node, E> {
        Ok(Node::Float(v))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Node, E> {
        Ok(Node::String(v.to_owned()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Node, E> {
        Ok(Node::String(v))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Node, E> {
        Err(E::custom("null is not allowed; omit the key instead"))
    }

    fn visit_none<E: de::Error>(self) -> Result<Node, E> {
        self.visit_unit()
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Node, D::Error> {
        d.deserialize_any(self)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Node::Seq(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
        let mut entries: Vec<(String, Node)> = Vec::new();
        while let Some(Key(key)) = map.next_key()? {
            if entries.iter().any(|(k, _)| *k == key) {
                return Err(de::Error::custom(format!("duplicate key `{key}`")));
            }
            let value = map.next_value()?;
            entries.push((key, value));
        }
        Ok(Node::Map(entries))
    }
}

/// A mapping key. Only strings are accepted, so that a YAML key such as `1`
/// or `0o17` is not silently turned into a different name.
struct Key(String);

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct KeyVisitor;
        impl Visitor<'_> for KeyVisitor {
            type Value = String;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string key")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
                Ok(v.to_owned())
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<String, E> {
                Ok(v)
            }
        }
        d.deserialize_any(KeyVisitor).map(Key)
    }
}

impl Node {
    fn invalid_type(&self, expected: &dyn Expected) -> Error {
        let unexpected = match self {
            Node::Bool(b) => Unexpected::Bool(*b),
            Node::Signed(n) => Unexpected::Signed(*n),
            Node::Unsigned(n) => Unexpected::Unsigned(*n),
            Node::Float(n) => Unexpected::Float(*n),
            Node::String(s) => Unexpected::Str(s),
            Node::Seq(_) => Unexpected::Seq,
            Node::Map(_) => Unexpected::Map,
        };
        de::Error::invalid_type(unexpected, expected)
    }
}

/// Reads the model from the tree. Unlike most deserializers, a struct is
/// read only from a mapping; serde would otherwise also read it from a
/// sequence by position.
impl<'de> Deserializer<'de> for Node {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self {
            Node::Bool(b) => visitor.visit_bool(b),
            Node::Signed(n) => visitor.visit_i64(n),
            Node::Unsigned(n) => visitor.visit_u64(n),
            Node::Float(n) => visitor.visit_f64(n),
            Node::String(s) => visitor.visit_string(s),
            Node::Seq(items) => {
                let mut seq = SeqDeserializer::new(items.into_iter());
                let value = visitor.visit_seq(&mut seq)?;
                seq.end()?;
                Ok(value)
            }
            Node::Map(entries) => {
                let mut map = MapDeserializer::new(entries.into_iter());
                let value = visitor.visit_map(&mut map)?;
                map.end()?;
                Ok(value)
            }
        }
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        match self {
            Node::Map(_) => self.deserialize_any(visitor),
            _ => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        match self {
            Node::String(s) => visitor.visit_enum(s.into_deserializer()),
            _ => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        // `null` never reaches the tree, so a present value is always `Some`.
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct seq tuple tuple_struct map identifier ignored_any
    }
}

impl IntoDeserializer<'_, Error> for Node {
    type Deserializer = Self;
    fn into_deserializer(self) -> Self {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(text: &str) -> Result<Option<Node>, String> {
        tree(text, Format::Yaml)
    }

    #[test]
    fn rejects_duplicate_keys_in_both_formats() {
        let e = yaml("a: 1\na: 2\n").unwrap_err();
        assert!(e.contains("duplicate key `a`"), "{e}");
        let e = tree(r#"{"a": 1, "a": 2}"#, Format::Json).unwrap_err();
        assert!(e.contains("duplicate key `a`"), "{e}");
    }

    #[test]
    fn rejects_non_string_keys() {
        for text in ["1: x\n", "0o17: x\n", "true: x\n"] {
            let e = yaml(text).unwrap_err();
            assert!(e.contains("expected a string key"), "{text}: {e}");
        }
    }

    #[test]
    fn rejects_null_inside_the_document() {
        for text in ["a: ~\n", "a: null\n", "a:\n", "- ~\n"] {
            let e = yaml(text).unwrap_err();
            assert!(e.contains("null is not allowed"), "{text}: {e}");
        }
        assert!(tree(r#"{"a": null}"#, Format::Json).is_err());
    }

    #[test]
    fn empty_document_has_no_tree() {
        assert_eq!(yaml(""), Ok(None));
        assert_eq!(yaml("# only a comment\n"), Ok(None));
        assert_eq!(tree("null", Format::Json), Ok(None));
    }

    #[test]
    fn keeps_scalar_types() {
        let node = yaml("a: 123\nb: '123'\nc: yes\nd: -1\ne: .inf\n")
            .unwrap()
            .unwrap();
        let Node::Map(entries) = node else { panic!() };
        assert_eq!(entries[0], ("a".to_owned(), Node::Unsigned(123)));
        assert_eq!(entries[1], ("b".to_owned(), Node::String("123".to_owned())));
        assert_eq!(entries[2], ("c".to_owned(), Node::String("yes".to_owned())));
        assert_eq!(entries[3], ("d".to_owned(), Node::Signed(-1)));
        assert_eq!(entries[4], ("e".to_owned(), Node::Float(f64::INFINITY)));
    }

    #[test]
    fn structs_are_read_only_from_mappings() {
        #[derive(Debug, serde::Deserialize)]
        #[expect(dead_code, reason = "only whether reading succeeds matters")]
        struct S {
            a: u64,
        }
        let seq = Node::Seq(vec![Node::Unsigned(1)]);
        let e = S::deserialize(seq).unwrap_err().to_string();
        assert!(e.contains("invalid type: sequence"), "{e}");
        let map = Node::Map(vec![("a".to_owned(), Node::Unsigned(1))]);
        assert!(S::deserialize(map).is_ok());
    }
}
