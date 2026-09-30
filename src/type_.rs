/// A specification for a type.
#[derive(Debug, Clone, PartialEq)]
pub struct Type {
    /// The value's name, used as the parameter name when a word is exposed as a tool.
    pub name: &'static str,
    /// The kind of the type.
    pub kind: TypeKind,
    /// The description for the type.
    pub description: String,
}
impl Type {
    pub fn new(name: &'static str, kind: TypeKind, description: &str) -> Self {
        Self {
            name,
            kind,
            description: description.to_string(),
        }
    }

    /// Creates a string type
    pub fn str(name: &'static str, description: &str) -> Self {
        Self::new(name, TypeKind::String, description)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    String,
    Int,
    Bool,
    List,
    Register,
    /// Only known at runtime, such as a value fetched from a register.
    Any,
}
impl TypeKind {
    /// Whether a value of type `got` can be used where `self` is expected.
    pub fn accepts(self, got: TypeKind) -> bool {
        self == TypeKind::Any || got == TypeKind::Any || self == got
    }
}
