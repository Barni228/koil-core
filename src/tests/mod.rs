use super::*;

fn add(f: &str) -> Action {
    Action::CreateFile(f.to_string())
}

fn delete(f: &str) -> Action {
    Action::DeleteFile(f.to_string())
}

fn rename(from: &str, to: &str) -> Action {
    Action::Rename(from.to_string(), to.to_string())
}

fn copy(from: &str, to: &str) -> Action {
    Action::Copy(from.to_string(), to.to_string())
}

mod test_planner;
