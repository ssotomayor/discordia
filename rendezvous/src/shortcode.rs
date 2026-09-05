use rand::Rng;
use rand::seq::SliceRandom;

const ADJECTIVES: &[&str] = &[
    "amber", "azure", "bold", "brave", "bright", "calm", "clever", "cool", "crisp", "deep",
    "eager", "fast", "fierce", "frosty", "gentle", "happy", "jolly", "keen", "lucky", "merry",
    "neat", "noble", "nimble", "ocean", "purple", "quiet", "quick", "rapid", "royal", "scarlet",
    "silent", "silver", "smooth", "snowy", "sparkly", "stormy", "sunny", "swift", "tidy", "vivid",
    "wild", "wise", "warm",
];

const ANIMALS: &[&str] = &[
    "ant", "bear", "bee", "bison", "cat", "cobra", "crab", "crow", "deer", "dolphin", "eagle",
    "elk", "falcon", "ferret", "fox", "frog", "goose", "hare", "hawk", "ibex", "jay", "koi",
    "lemur", "lynx", "moose", "newt", "otter", "owl", "panda", "panther", "puma", "quail", "raven",
    "salmon", "seal", "shrew", "stoat", "swan", "tiger", "toad", "viper", "weasel", "whale",
    "wolf", "wren", "yak",
];

pub fn generate() -> String {
    let mut rng = rand::thread_rng();
    let adj = ADJECTIVES.choose(&mut rng).unwrap_or(&"swift");
    let animal = ANIMALS.choose(&mut rng).unwrap_or(&"fox");
    let num: u16 = rng.gen_range(1000..10_000);
    format!("{adj}-{animal}-{num}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_looks_right() {
        for _ in 0..200 {
            let c = generate();
            let parts: Vec<&str> = c.split('-').collect();
            assert_eq!(parts.len(), 3, "{c}");
            assert!(ADJECTIVES.contains(&parts[0]), "{c}");
            assert!(ANIMALS.contains(&parts[1]), "{c}");
            assert_eq!(parts[2].len(), 4, "{c}");
            let n: u16 = parts[2].parse().unwrap();
            assert!((1000..=9999).contains(&n), "{c}");
        }
    }
}
