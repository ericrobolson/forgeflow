//! A reproducible JSON-lines echo of the committed database.
use super::{schema::*, store::Store, world::rows};
/// Renders the event, aspect, presence and fate rows in deterministic order.
pub fn render(db: &rusqlite::Connection) -> Result<String, String> {
    let mut text = String::new();
    for row in rows(
        db,
        &format!("SELECT {} FROM events ORDER BY id", EventRow::COLUMNS),
        [],
        EventRow::from_row,
    )? {
        text +=
            &serde_json::to_string(&serde_json::json!({"event":row})).map_err(|e| e.to_string())?;
        text.push('\n');
    }
    for row in rows(
        db,
        &format!("SELECT {} FROM aspects ORDER BY id", AspectRow::COLUMNS),
        [],
        AspectRow::from_row,
    )? {
        text += &serde_json::to_string(&serde_json::json!({"aspect":row}))
            .map_err(|e| e.to_string())?;
        text.push('\n');
    }
    for row in rows(
        db,
        &format!("SELECT {} FROM presence ORDER BY id", PresenceRow::COLUMNS),
        [],
        PresenceRow::from_row,
    )? {
        text += &serde_json::to_string(&serde_json::json!({"presence":row}))
            .map_err(|e| e.to_string())?;
        text.push('\n');
    }
    for row in rows(
        db,
        &format!(
            "SELECT {} FROM fate_transactions ORDER BY id",
            FateRow::COLUMNS
        ),
        [],
        FateRow::from_row,
    )? {
        text +=
            &serde_json::to_string(&serde_json::json!({"fate":row})).map_err(|e| e.to_string())?;
        text.push('\n');
    }
    Ok(text)
}
/// Rewrites events.jsonl entirely from world truth.
pub fn write(store: &Store) -> Result<(), String> {
    std::fs::write(store.dir().join("events.jsonl"), render(store.db())?).map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpg::test_support::{test_roles, test_session};
    #[test]
    fn rebuild_restores_a_drifted_echo() {
        let s = test_session("echo-rebuild", test_roles());
        std::fs::write(s.store.dir().join("events.jsonl"), "junk").unwrap();
        write(&s.store).unwrap();
        assert_eq!(
            std::fs::read_to_string(s.store.dir().join("events.jsonl")).unwrap(),
            render(s.store.db()).unwrap()
        );
        let root = s.context.project.root.clone();
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn echo_lists_rows_in_order() {
        let s = test_session("echo-order", test_roles());
        s.store.db().execute_batch("INSERT INTO events(id,turn,game_time,summary) VALUES(2,1,'1471-03-12T20:00','two'),(1,1,'1471-03-12T20:00','one')").unwrap();
        let text = render(s.store.db()).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(text.lines().next().unwrap()).unwrap()["event"]
                ["id"],
            1
        );
        let root = s.context.project.root.clone();
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }
}
