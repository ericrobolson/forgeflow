use std::path::PathBuf;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::value::Value;

pub const REGISTER_COUNT: usize = 16;
/// R0-R3 persist per project; R4-R15 are scratch for the session.
pub const DURABLE_COUNT: usize = 4;
/// Durable registers hold notes, not whole files.
pub const DURABLE_MAX_CHARS: usize = 2000;
const PREVIEW_CHARS: usize = 120;
pub const MEMORY_FILE: &str = "memory.json";

/// Named persistent cells addressed by user words such as `score`.
#[derive(Debug)]
pub struct Memory { cells: HashMap<String, Option<Value>>, path: Option<PathBuf> }
impl Memory {
    pub fn in_memory() -> Self { Self { cells: HashMap::new(), path: None } }
    pub fn open(path: PathBuf) -> Result<Self, String> {
        let cells = if path.exists() { serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?).map_err(|e| format!("invalid durable memory: {e}"))? } else { HashMap::new() };
        Ok(Self { cells, path: Some(path) })
    }
    pub fn address(&mut self, name: &str) -> Value { self.cells.entry(name.to_string()).or_insert(None); Value::Address(name.to_string()) }
    pub fn get(&self, name: &str) -> Result<&Value, String> { self.cells.get(name).and_then(Option::as_ref).ok_or_else(|| format!("address `{name}` is uninitialized")) }
    pub fn set(&mut self, name: &str, value: Value) -> Result<(), String> {
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '?') {
            return Err(format!("invalid durable address name `{name}`"));
        }
        let old = self.cells.insert(name.to_string(), Some(value));
        if let Err(error) = self.persist() { if let Some(old) = old { self.cells.insert(name.to_string(), old); } else { self.cells.remove(name); } return Err(error); }
        Ok(())
    }
    fn persist(&self) -> Result<(), String> {
        let Some(path) = &self.path else { return Ok(()); };
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
        let bytes = serde_json::to_vec_pretty(&self.cells).map_err(|e| e.to_string())?;
        let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        let temporary = parent.join(format!(".memory-{}.tmp", uuid::Uuid::now_v7()));
        let result = (|| {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary).map_err(|e| e.to_string())?;
            file.write_all(&bytes).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            std::fs::rename(&temporary, path).map_err(|e| e.to_string())
        })();
        if result.is_err() { let _ = std::fs::remove_file(&temporary); }
        result
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slot {
    pub value: Value,
    /// What produced the value, such as `read_file src/main.rs`.
    pub source: String,
    #[serde(skip)]
    last_used: u64,
}

#[derive(Debug)]
pub struct Registers {
    slots: [Option<Slot>; REGISTER_COUNT],
    clock: u64,
    /// Where durable registers persist; `None` keeps everything in memory.
    path: Option<PathBuf>,
}

/// Where a scratch allocation landed, and what it replaced.
#[derive(Debug, PartialEq)]
pub struct Allocation {
    pub register: usize,
    pub replaced: Option<String>,
}

impl Registers {
    pub fn in_memory() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            clock: 0,
            path: None,
        }
    }

    /// Loads durable registers from `path`, if it exists.
    pub fn open(path: PathBuf) -> Result<Self, String> {
        let mut registers = Self::in_memory();
        if path.exists() {
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            let durable: Vec<Option<Slot>> =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            for (slot, value) in registers.slots.iter_mut().zip(durable) {
                *slot = value;
            }
        }
        registers.path = Some(path);
        Ok(registers)
    }

    pub fn is_durable(register: usize) -> bool {
        register < DURABLE_COUNT
    }

    pub fn get(&mut self, register: usize) -> Result<&Value, String> {
        check_index(register)?;
        self.clock += 1;
        let clock = self.clock;
        let slot = self.slots[register]
            .as_mut()
            .ok_or_else(|| format!("R{register} is empty"))?;
        slot.last_used = clock;
        Ok(&slot.value)
    }

    pub fn set(&mut self, register: usize, value: Value, source: &str) -> Result<(), String> {
        check_index(register)?;
        if Self::is_durable(register) && value.to_text().chars().count() > DURABLE_MAX_CHARS {
            return Err(format!(
                "R{register} is durable and holds at most {DURABLE_MAX_CHARS} characters; store a summary instead"
            ));
        }
        let old_slot = self.slots[register].clone();
        let old_clock = self.clock;
        self.clock += 1;
        self.slots[register] = Some(Slot {
            value,
            source: source.to_string(),
            last_used: self.clock,
        });
        if Self::is_durable(register) {
            if let Err(error) = self.persist() {
                self.slots[register] = old_slot;
                self.clock = old_clock;
                return Err(error);
            }
        }
        Ok(())
    }

    /// Replaces what a register says produced its value.
    pub fn relabel(&mut self, register: usize, source: &str) -> Result<(), String> {
        check_index(register)?;
        let old_slot = self.slots[register].clone();
        if let Some(slot) = self.slots[register].as_mut() {
            slot.source = source.to_string();
        }
        if Self::is_durable(register) {
            if let Err(error) = self.persist() {
                self.slots[register] = old_slot;
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn clear(&mut self, register: usize) -> Result<(), String> {
        check_index(register)?;
        let old_slot = self.slots[register].clone();
        self.slots[register] = None;
        if Self::is_durable(register) {
            if let Err(error) = self.persist() {
                self.slots[register] = old_slot;
                return Err(error);
            }
        }
        Ok(())
    }

    /// Places a value in an empty scratch register, or replaces the least recently used one.
    pub fn allocate(&mut self, value: Value, source: &str) -> Allocation {
        let scratch = DURABLE_COUNT..REGISTER_COUNT;
        let register = scratch
            .clone()
            .find(|&r| self.slots[r].is_none())
            .unwrap_or_else(|| {
                scratch
                    .min_by_key(|&r| self.slots[r].as_ref().map_or(0, |s| s.last_used))
                    .unwrap_or(DURABLE_COUNT)
            });
        let replaced = self.slots[register].as_ref().map(|s| s.source.clone());
        self.clock += 1;
        self.slots[register] = Some(Slot {
            value,
            source: source.to_string(),
            last_used: self.clock,
        });
        Allocation { register, replaced }
    }

    /// A one-line receipt for a register, such as `R5 text 4.2k "fn main() {…"`.
    pub fn receipt(&self, register: usize) -> String {
        match &self.slots.get(register).and_then(Option::as_ref) {
            Some(slot) => format!("R{register} {} {}", slot.value.describe(), preview(&slot.value)),
            None => format!("R{register} empty"),
        }
    }

    /// The register table shown to the model each turn.
    pub fn table(&self) -> String {
        let mut rows = vec![];
        for (register, slot) in self.slots.iter().enumerate() {
            let scope = if Self::is_durable(register) { "durable" } else { "scratch" };
            match slot {
                Some(slot) => rows.push(format!(
                    "R{register} {scope} {} [{}] {}",
                    slot.value.describe(),
                    slot.source,
                    preview(&slot.value)
                )),
                None if Self::is_durable(register) => rows.push(format!("R{register} {scope} empty")),
                None => {}
            }
        }
        let empty_scratch = (DURABLE_COUNT..REGISTER_COUNT)
            .filter(|&r| self.slots[r].is_none())
            .count();
        rows.push(format!("({empty_scratch} scratch registers empty)"));
        rows.join("\n")
    }

    pub fn is_filled(&self, register: usize) -> bool {
        self.slots.get(register).is_some_and(Option::is_some)
    }

    /// Occupied registers only, one per line; empty when nothing is stored.
    pub fn summary(&self) -> String {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(register, slot)| {
                let slot = slot.as_ref()?;
                Some(format!("R{register} {} [{}] {}", slot.value.describe(), slot.source, preview(&slot.value)))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn persist(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let durable = &self.slots[..DURABLE_COUNT];
        let json = serde_json::to_vec_pretty(durable).map_err(|e| e.to_string())?;
        let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        let temporary = parent.join(format!(".registers-{}.tmp", uuid::Uuid::now_v7()));
        let result = (|| {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary).map_err(|e| e.to_string())?;
            file.write_all(&json).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            std::fs::rename(&temporary, path).map_err(|e| e.to_string())
        })();
        if result.is_err() { let _ = std::fs::remove_file(&temporary); }
        result
    }
}

fn check_index(register: usize) -> Result<(), String> {
    if register < REGISTER_COUNT {
        Ok(())
    } else {
        Err(format!("registers are R0 through R{}", REGISTER_COUNT - 1))
    }
}

/// Parses `R5`, `r5`, or `$R5` into a register index.
pub fn parse_register(text: &str) -> Option<usize> {
    let text = text.strip_prefix('$').unwrap_or(text);
    let digits = text.strip_prefix(['R', 'r'])?;
    let register: usize = digits.parse().ok()?;
    (register < REGISTER_COUNT).then_some(register)
}

/// The start of a value, quoted on one line; marked complete when nothing was cut.
fn preview(value: &Value) -> String {
    let text = value.to_text().replace('\n', "⏎");
    let count = text.chars().count();
    if count <= PREVIEW_CHARS {
        format!("\"{text}\" (complete)")
    } else {
        let head: String = text.chars().take(PREVIEW_CHARS).collect();
        format!("\"{head}…\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_memory_persists_values_and_addresses_by_name() {
        let path = std::env::temp_dir().join(format!("forgeflow-memory-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut memory = Memory::open(path.clone()).unwrap();
        assert_eq!(memory.address("score"), Value::Address("score".into()));
        memory.set("score", Value::Int(42)).unwrap();
        drop(memory);
        let reopened = Memory::open(path.clone()).unwrap();
        assert_eq!(reopened.get("score"), Ok(&Value::Int(42)));
        std::fs::remove_file(path).unwrap();
    }

    fn text(s: &str) -> Value {
        Value::String(s.to_string())
    }

    #[test]
    fn allocation_fills_scratch_then_replaces_least_recently_used() {
        let mut registers = Registers::in_memory();
        for i in 0..(REGISTER_COUNT - DURABLE_COUNT) {
            let allocation = registers.allocate(text("x"), &format!("source {i}"));
            assert_eq!(allocation.register, DURABLE_COUNT + i);
            assert_eq!(allocation.replaced, None);
        }
        // Touch R4 so R5 becomes the least recently used.
        registers.get(4).unwrap();
        let allocation = registers.allocate(text("y"), "new");
        assert_eq!(allocation.register, 5);
        assert_eq!(allocation.replaced, Some("source 1".into()));
    }

    #[test]
    fn allocation_never_touches_durable_registers() {
        let mut registers = Registers::in_memory();
        for _ in 0..40 {
            assert!(registers.allocate(text("x"), "s").register >= DURABLE_COUNT);
        }
    }

    #[test]
    fn durable_registers_cap_their_size() {
        let mut registers = Registers::in_memory();
        let long = text(&"a".repeat(DURABLE_MAX_CHARS + 1));
        assert!(registers.set(0, long.clone(), "s").is_err());
        assert!(registers.set(DURABLE_COUNT, long, "s").is_ok());
    }

    #[test]
    fn durable_registers_persist_and_scratch_does_not() {
        let path = std::env::temp_dir().join(format!("forgeflow-registers-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut registers = Registers::open(path.clone()).unwrap();
        registers.set(1, text("note"), "reg_store").unwrap();
        registers.set(7, text("scratch"), "read_file").unwrap();
        let mut reopened = Registers::open(path.clone()).unwrap();
        assert_eq!(reopened.get(1).unwrap(), &text("note"));
        assert!(reopened.get(7).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn parses_register_names() {
        let cases = [
            ("R0", Some(0)),
            ("r15", Some(15)),
            ("$R5", Some(5)),
            ("R16", None),
            ("R", None),
            ("5", None),
            ("src/R5", None),
        ];
        for (input, expected) in cases {
            assert_eq!(parse_register(input), expected, "{input}");
        }
    }

    #[test]
    fn receipts_mark_short_values_complete() {
        let mut registers = Registers::in_memory();
        registers.set(4, text("hi\nthere"), "s").unwrap();
        assert_eq!(registers.receipt(4), "R4 text 8 chars, 2 lines \"hi⏎there\" (complete)");
        registers.set(5, text(&"a".repeat(500)), "s").unwrap();
        assert!(registers.receipt(5).ends_with("…\""));
    }
}
