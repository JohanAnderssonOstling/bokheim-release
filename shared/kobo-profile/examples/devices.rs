use kobo_profile::PROFILES;

fn main() {
    if std::env::args().any(|arg| arg == "--ids") {
        println!("{}", PROFILES.iter().map(|profile| profile.id.to_string()).collect::<Vec<_>>().join(","));
    } else {
        for profile in PROFILES {
            println!("{}\t{}\t{}x{}\t{}", profile.id, profile.product, profile.width, profile.height, profile.name);
        }
    }
}
