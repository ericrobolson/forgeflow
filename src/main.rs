use forgeflow::{project::Project, util};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let working_dir = std::env::current_dir().unwrap();

    if !Project::exists() && !util::confirmation("Project not found. Create one?") {
        println!("Not creating project.");
        return;
    }

    let project = Project::initialize(&working_dir);
}
