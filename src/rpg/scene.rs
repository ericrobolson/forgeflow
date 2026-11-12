//! Deterministic scene descriptions for image and video generation.
use super::scenes::SceneContext;
/// Assembles scene facts and optional tone into a visual prompt.
pub fn scene_prompt(ctx: &SceneContext, tense: Option<bool>) -> String {
    let mut parts = vec![];
    if let Some(t) = tense {
        parts.push(
            if t {
                "tense, dramatic lighting"
            } else {
                "calm, atmospheric"
            }
            .into(),
        );
    }
    parts.push(
        ctx.scene
            .as_ref()
            .map(|s| {
                s.summary
                    .as_deref()
                    .unwrap_or(&s.title)
                    .trim_end_matches('.')
                    .to_owned()
            })
            .unwrap_or_else(|| "an empty landscape".into()),
    );
    parts.extend(ctx.situation_aspects.iter().map(|a| a.text.to_lowercase()));
    if let Some(l) = &ctx.location {
        parts.push(l.name.clone());
        parts.extend(ctx.location_aspects.iter().map(|a| a.text.to_lowercase()));
    }
    for p in &ctx.present {
        let aspects = p
            .aspects
            .iter()
            .take(2)
            .map(|a| a.text.to_lowercase())
            .collect::<Vec<_>>()
            .join(" and ");
        parts.push(if aspects.is_empty() {
            p.entity.name.clone()
        } else {
            format!("{}, {aspects}", p.entity.name)
        });
    }
    parts.push("cinematic fantasy illustration".into());
    parts.join(", ")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scene_prompt_without_a_scene() {
        assert_eq!(
            scene_prompt(&SceneContext::default(), None),
            "an empty landscape, cinematic fantasy illustration"
        );
    }
    #[test]
    fn scene_prompt_is_deterministic() {
        let c = crate::rpg::test_support::test_context("scene-prompt");
        let s = crate::rpg::store::Store::create(&c.project, "Sample").unwrap();
        let mut m = s.meta().clone();
        crate::rpg::seed::sample(&mut crate::rpg::world::World {
            db: s.db(),
            meta: &mut m,
            changes: vec![],
        })
        .unwrap();
        let ctx =
            crate::rpg::scenes::get_scene_context(s.db(), &m, m.current_scene_id, vec![]).unwrap();
        assert_eq!(
            scene_prompt(&ctx, Some(true)),
            "tense, dramatic lighting, Rain lashes the old bridge as Gareth bars the way into Ashford, rain-slicked stones, torches guttering in the wind, Ashford Bridge, ancient and crumbling, Aldric Vane, disgraced knight of the ember crown and hot-headed, Sera Quickfingers, charming thief with a conscience and owes the guild everything, Gareth, bridge warden of ashford and sworn to the old king, cinematic fantasy illustration"
        );
        drop(s);
        std::fs::remove_dir_all(c.project.root).unwrap();
    }
}
