/// A specification for a type.
#[derive(Debug, Clone, PartialEq)]
pub struct Type {
    /// The kind of the type.
    pub kind: TypeKind,
    /// The description for the type.
    pub description: String,
}
impl Type {
    /// Creates a string type
    pub fn str(description: &str) -> Self {
        Self {
            kind: TypeKind::String,
            description: description.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeKind {
    String,
}
