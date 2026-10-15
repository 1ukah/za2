//! Game icons for the UI, loaded from the assets folder on first use and kept for the run. A missing file gives an
//! empty image, which Slint draws as nothing.

use std::cell::RefCell;
use std::collections::HashMap;

use slint::Image;

thread_local! {
    static CACHE: RefCell<HashMap<String, Image>> = RefCell::new(HashMap::new());
}

/// The image at `relative` (to the assets folder); empty when there is no such file.
pub fn load(relative: &str) -> Image {
    CACHE.with(|cache| {
        cache
            .borrow_mut()
            .entry(relative.to_string())
            .or_insert_with(|| Image::load_from_path(&za2_core::assets::path(relative)).unwrap_or_default())
            .clone()
    })
}

/// The class emblem file of a class. Spiritmaster and Brawler have the game's internal names.
pub fn class_file(class: &str) -> Option<&'static str> {
    Some(match class {
        "Gladiator" => "classes/gladiator.png",
        "Templar" => "classes/templar.png",
        "Assassin" => "classes/assassin.png",
        "Ranger" => "classes/ranger.png",
        "Sorcerer" => "classes/sorcerer.png",
        "Spiritmaster" => "classes/elementalist.png",
        "Cleric" => "classes/cleric.png",
        "Chanter" => "classes/chanter.png",
        "Brawler" => "classes/fighter.png",
        _ => return None,
    })
}

pub fn class_icon(class: Option<&str>) -> Image {
    class.and_then(class_file).map(load).unwrap_or_default()
}

pub fn skill_icon(skill_id: i32) -> Image {
    za2_core::catalog::skills::icon_of(skill_id).map(|p| load(&p)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_class_has_an_emblem() {
        for class in ["Gladiator", "Templar", "Assassin", "Ranger", "Sorcerer", "Spiritmaster", "Cleric", "Chanter", "Brawler"] {
            let file = class_file(class).unwrap();
            assert!(za2_core::assets::path(file).is_file(), "{file}");
        }
    }
}
