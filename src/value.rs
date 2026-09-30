use serde::{Deserialize, Serialize};

use crate::type_::TypeKind;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Value {
    String(String),
    Int(i64),
    Bool(bool),
    List(Vec<Value>),
    Register(usize),
}
impl Value {
    pub fn expect_string(&self) -> Result<String, String> {
        match self {
            Value::String(s) => Ok(s.to_string()),
            other => Err(format!("expected a string, got {}", other.describe())),
        }
    }

    pub fn expect_int(&self) -> Result<i64, String> {
        match self {
            Value::Int(i) => Ok(*i),
            other => Err(format!("expected an integer, got {}", other.describe())),
        }
    }

    pub fn expect_register(&self) -> Result<usize, String> {
        match self {
            Value::Register(r) => Ok(*r),
            other => Err(format!("expected a register, got {}", other.describe())),
        }
    }

    pub fn kind(&self) -> TypeKind {
        match self {
            Value::String(_) => TypeKind::String,
            Value::Int(_) => TypeKind::Int,
            Value::Bool(_) => TypeKind::Bool,
            Value::List(_) => TypeKind::List,
            Value::Register(_) => TypeKind::Register,
        }
    }

    /// The value as plain text; lists become one item per line.
    pub fn to_text(&self) -> String {
        match self {
            Value::String(s) => s.clone(),
            Value::Int(i) => i.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::List(items) => items
                .iter()
                .map(Value::to_text)
                .collect::<Vec<_>>()
                .join("\n"),
            Value::Register(r) => format!("R{r}"),
        }
    }

    /// A short type-and-size label, such as `text 4.2k chars, 120 lines` or `list(6)`.
    pub fn describe(&self) -> String {
        match self {
            Value::String(s) => {
                let lines = s.lines().count();
                format!(
                    "text {} chars, {lines} line{}",
                    human_count(s.chars().count()),
                    if lines == 1 { "" } else { "s" }
                )
            }
            Value::Int(_) => "int".into(),
            Value::Bool(_) => "bool".into(),
            Value::List(items) => format!("list({})", items.len()),
            Value::Register(r) => format!("register R{r}"),
        }
    }
}

fn human_count(n: usize) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}
