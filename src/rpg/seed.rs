//! The Crown of Ashes sample world.
use super::{
    scenes::{Participant, SceneInput},
    world::{AspectInput, Scope, World},
};
/// Builds the validated sample cast and opening bridge scene.
pub fn sample(w: &mut World) -> Result<(), String> {
    let e1 = w.add_entity("Aldric Vane", "character", None)?;
    w.add_aspect(AspectInput {
        text: "Disgraced Knight of the Ember Crown".into(),
        scope: Scope::Character,
        holder_id: Some(e1),
        slot: Some("high-concept".into()),
        valid_from: None,
    })?;
    w.add_aspect(AspectInput {
        text: "Hot-headed".into(),
        scope: Scope::Character,
        holder_id: Some(e1),
        slot: Some("trouble".into()),
        valid_from: None,
    })?;
    w.set_skill(e1, "Fight", 3, None, None)?;
    w.set_skill(e1, "Will", 2, None, None)?;
    w.set_skill(e1, "Notice", 1, None, None)?;
    w.add_stress_track(e1, "Physical", 3)?;
    w.add_stress_track(e1, "Mental", 2)?;
    w.award_fate(e1, 3, "refresh", None)?;
    let e2 = w.add_entity("Sera Quickfingers", "character", None)?;
    w.add_aspect(AspectInput {
        text: "Charming Thief With a Conscience".into(),
        scope: Scope::Character,
        holder_id: Some(e2),
        slot: Some("high-concept".into()),
        valid_from: None,
    })?;
    w.add_aspect(AspectInput {
        text: "Owes the Guild Everything".into(),
        scope: Scope::Character,
        holder_id: Some(e2),
        slot: Some("trouble".into()),
        valid_from: None,
    })?;
    w.set_skill(e2, "Burglary", 3, None, None)?;
    w.set_skill(e2, "Rapport", 2, None, None)?;
    w.set_skill(e2, "Stealth", 2, None, None)?;
    w.add_stress_track(e2, "Physical", 2)?;
    w.add_stress_track(e2, "Mental", 3)?;
    w.award_fate(e2, 3, "refresh", None)?;
    let e3 = w.add_entity("Gareth", "character", None)?;
    w.add_aspect(AspectInput {
        text: "Bridge Warden of Ashford".into(),
        scope: Scope::Character,
        holder_id: Some(e3),
        slot: Some("high-concept".into()),
        valid_from: None,
    })?;
    w.add_aspect(AspectInput {
        text: "Sworn to the Old King".into(),
        scope: Scope::Character,
        holder_id: Some(e3),
        slot: Some("trouble".into()),
        valid_from: None,
    })?;
    w.set_skill(e3, "Fight", 2, None, None)?;
    w.add_stress_track(e3, "Physical", 2)?;
    let e4 = w.add_entity("Mother Ilse", "character", None)?;
    w.add_aspect(AspectInput {
        text: "Hedge Witch Who Sees Too Much".into(),
        scope: Scope::Character,
        holder_id: Some(e4),
        slot: Some("high-concept".into()),
        valid_from: None,
    })?;
    w.set_skill(e4, "Lore", 3, None, None)?;
    let e5 = w.add_entity("The Rat King", "character", None)?;
    w.add_aspect(AspectInput {
        text: "Lord of the Undercity".into(),
        scope: Scope::Character,
        holder_id: Some(e5),
        slot: Some("high-concept".into()),
        valid_from: None,
    })?;
    w.set_skill(e5, "Deceive", 4, None, None)?;
    w.add_behavior(
        e3,
        "disposition",
        "when_betrayed",
        "attacks without warning",
        0,
    )?;
    let location = w.add_entity("Ashford Bridge", "location", None)?;
    w.add_aspect(AspectInput {
        text: "Ancient and Crumbling".into(),
        scope: Scope::Situation,
        holder_id: Some(location),
        slot: None,
        valid_from: None,
    })?;
    w.add_aspect(AspectInput {
        text: "The Empire Is Falling".into(),
        scope: Scope::Campaign,
        holder_id: None,
        slot: None,
        valid_from: None,
    })?;
    w.update_relationship(e3, e1, "enemy", -10, None)?;
    w.update_relationship(e2, e5, "owes", -20, None)?;
    w.update_relationship(e4, e2, "ally", 30, None)?;
    w.begin_scene(SceneInput {
        title: "The Bridge at Midnight".into(),
        summary: Some("Rain lashes the old bridge as Gareth bars the way into Ashford.".into()),
        location_id: Some(location),
        chapter: Some(1),
        participants: vec![
            Participant {
                entity_id: e1,
                role: "pc".into(),
            },
            Participant {
                entity_id: e2,
                role: "pc".into(),
            },
            Participant {
                entity_id: e3,
                role: "npc".into(),
            },
        ],
        situation_aspects: vec![
            "Rain-slicked Stones".into(),
            "Torches Guttering in the Wind".into(),
        ],
    })?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_sample_seed_has_an_opening_scene() {
        let c = crate::rpg::test_support::test_context("sample");
        let s = crate::rpg::store::Store::create(&c.project, "Sample").unwrap();
        let mut meta = s.meta().clone();
        let mut w = World {
            db: s.db(),
            meta: &mut meta,
            changes: vec![],
        };
        sample(&mut w).unwrap();
        let ctx =
            crate::rpg::scenes::get_scene_context(s.db(), w.meta, w.meta.current_scene_id, vec![])
                .unwrap();
        assert_eq!(ctx.present.iter().filter(|p| p.role == "pc").count(), 2);
        assert!(ctx.render().contains("Hot-headed"));
        drop(s);
        std::fs::remove_dir_all(c.project.root).unwrap();
    }
}
