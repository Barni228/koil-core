use super::*;

fn add(f: &str) -> Action {
    Action::CreateFile(f.into())
}

fn delete(f: &str) -> Action {
    Action::DeleteFile(f.into())
}

fn rename(from: &str, to: &str) -> Action {
    Action::Rename(from.into(), to.into())
}

fn copy(from: &str, to: &str) -> Action {
    Action::Copy(from.into(), to.into())
}

mod test_diff;
mod test_planner;
