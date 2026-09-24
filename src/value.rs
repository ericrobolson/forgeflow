#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    String(String),
}
impl Value {
    pub fn expect_string(&self) -> Result<String, String> {
        match self {
            Value::String(s) => Ok(s.to_string()),
        }
    }
}
